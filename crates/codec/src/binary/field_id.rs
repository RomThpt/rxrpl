use crate::error::CodecError;

/// Encode a field ID (type_code, field_code) into 1-3 bytes.
///
/// XRPL field ID encoding:
/// - If both type_code < 16 and field_code < 16: 1 byte (type_code << 4 | field_code)
/// - If type_code >= 16 and field_code < 16: 2 bytes (field_code, type_code)
/// - If type_code < 16 and field_code >= 16: 2 bytes (type_code << 4, field_code)
/// - If both >= 16: 3 bytes (0x00, type_code, field_code)
pub fn encode_field_id(type_code: i32, field_code: i32) -> Vec<u8> {
    let tc = type_code as u8;
    let fc = field_code as u8;

    if type_code < 16 && field_code < 16 {
        vec![(tc << 4) | fc]
    } else if type_code >= 16 && field_code < 16 {
        vec![fc, tc]
    } else if type_code < 16 && field_code >= 16 {
        vec![tc << 4, fc]
    } else {
        vec![0x00, tc, fc]
    }
}

/// Decode a field ID from bytes, returning (type_code, field_code, bytes_consumed).
pub fn decode_field_id(data: &[u8]) -> Result<(i32, i32, usize), CodecError> {
    if data.is_empty() {
        return Err(CodecError::UnexpectedEnd);
    }

    let byte0 = data[0];
    let type_code = (byte0 >> 4) as i32;
    let field_code = (byte0 & 0x0F) as i32;

    if type_code != 0 && field_code != 0 {
        // Both fit in the first byte
        Ok((type_code, field_code, 1))
    } else if type_code == 0 && field_code == 0 {
        // Both >= 16: 3 bytes
        if data.len() < 3 {
            return Err(CodecError::UnexpectedEnd);
        }
        Ok((data[1] as i32, data[2] as i32, 3))
    } else if type_code == 0 {
        // type >= 16, field < 16
        if data.len() < 2 {
            return Err(CodecError::UnexpectedEnd);
        }
        Ok((data[1] as i32, field_code, 2))
    } else {
        // type < 16, field >= 16
        if data.len() < 2 {
            return Err(CodecError::UnexpectedEnd);
        }
        Ok((type_code, data[1] as i32, 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_small() {
        // Both < 16: e.g., UInt16 (type=1), TransactionType (field=2)
        let encoded = encode_field_id(1, 2);
        assert_eq!(encoded, vec![0x12]);
        let (tc, fc, len) = decode_field_id(&encoded).unwrap();
        assert_eq!((tc, fc, len), (1, 2, 1));
    }

    #[test]
    fn encode_decode_large_type() {
        // type >= 16, field < 16
        let encoded = encode_field_id(17, 1);
        assert_eq!(encoded, vec![0x01, 17]);
        let (tc, fc, len) = decode_field_id(&encoded).unwrap();
        assert_eq!((tc, fc, len), (17, 1, 2));
    }

    #[test]
    fn encode_decode_large_field() {
        // type < 16, field >= 16
        let encoded = encode_field_id(1, 16);
        assert_eq!(encoded, vec![0x10, 16]);
        let (tc, fc, len) = decode_field_id(&encoded).unwrap();
        assert_eq!((tc, fc, len), (1, 16, 2));
    }

    #[test]
    fn encode_decode_both_large() {
        let encoded = encode_field_id(18, 20);
        assert_eq!(encoded, vec![0x00, 18, 20]);
        let (tc, fc, len) = decode_field_id(&encoded).unwrap();
        assert_eq!((tc, fc, len), (18, 20, 3));
    }
}
