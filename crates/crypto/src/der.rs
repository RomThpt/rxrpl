use thiserror::Error;

#[derive(Debug, Error)]
pub enum DerError {
    #[error("invalid DER: not enough data")]
    NotEnoughData,
    #[error("invalid DER: expected integer tag 0x02")]
    InvalidIntegerTag,
    #[error("invalid DER: incorrect length")]
    IncorrectLength,
    #[error("invalid DER: leftover bytes after parsing")]
    LeftoverBytes,
}

/// Encode two big-integer byte slices (r, s) into DER format.
pub fn encode_der_signature(r: &[u8], s: &[u8]) -> Vec<u8> {
    let r_padded = pad_integer(r);
    let s_padded = pad_integer(s);

    let total_len = 2 + r_padded.len() + 2 + s_padded.len();
    let mut out = Vec::with_capacity(2 + total_len);
    out.push(0x30); // SEQUENCE
    out.push(total_len as u8);
    out.push(0x02); // INTEGER
    out.push(r_padded.len() as u8);
    out.extend_from_slice(&r_padded);
    out.push(0x02); // INTEGER
    out.push(s_padded.len() as u8);
    out.extend_from_slice(&s_padded);
    out
}

/// Decode a DER-encoded signature into (r, s) byte slices.
pub fn decode_der_signature(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), DerError> {
    if data.len() < 2 {
        return Err(DerError::NotEnoughData);
    }
    if data[0] != 0x30 {
        return Err(DerError::IncorrectLength);
    }
    let seq_len = data[1] as usize;
    if seq_len != data.len() - 2 {
        return Err(DerError::IncorrectLength);
    }

    let (r, rest) = parse_integer(&data[2..])?;
    let (s, rest) = parse_integer(rest)?;

    if !rest.is_empty() {
        return Err(DerError::LeftoverBytes);
    }

    Ok((r, s))
}

fn parse_integer(data: &[u8]) -> Result<(Vec<u8>, &[u8]), DerError> {
    if data.len() < 2 {
        return Err(DerError::NotEnoughData);
    }
    if data[0] != 0x02 {
        return Err(DerError::InvalidIntegerTag);
    }
    let len = data[1] as usize;
    if data.len() < 2 + len {
        return Err(DerError::NotEnoughData);
    }
    let integer_bytes = &data[2..2 + len];
    // Strip leading zero padding
    let stripped = strip_leading_zeros(integer_bytes);
    Ok((stripped.to_vec(), &data[2 + len..]))
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < bytes.len() - 1 && bytes[i] == 0 {
        i += 1;
    }
    &bytes[i..]
}

/// Add a leading 0x00 byte if the high bit is set (to keep the integer positive in DER).
fn pad_integer(bytes: &[u8]) -> Vec<u8> {
    // Strip any existing leading zeros first
    let stripped = strip_leading_zeros(bytes);
    if stripped[0] & 0x80 != 0 {
        let mut padded = Vec::with_capacity(1 + stripped.len());
        padded.push(0x00);
        padded.extend_from_slice(stripped);
        padded
    } else {
        stripped.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_der() {
        let r = vec![
            0x58, 0x3A, 0x91, 0xC9, 0x5E, 0x54, 0xE6, 0xA6, 0x51, 0xC4, 0x7B, 0xEC, 0x22, 0x74,
            0x4E, 0x0B, 0x10, 0x1E, 0x2C, 0x40, 0x60, 0xE7, 0xB0, 0x8F, 0x63, 0x41, 0x65, 0x7D,
            0xAD, 0x9B, 0xC3, 0xEE,
        ];
        let s = vec![
            0x7D, 0x14, 0x89, 0xC7, 0x39, 0x5D, 0xB0, 0x18, 0x8D, 0x3A, 0x56, 0xA9, 0x77, 0xEC,
            0xBA, 0x54, 0xB3, 0x6F, 0xA9, 0x37, 0x1B, 0x40, 0x31, 0x96, 0x55, 0xB1, 0xB4, 0x42,
            0x9E, 0x33, 0xEF, 0x2D,
        ];

        let der = encode_der_signature(&r, &s);
        let (r2, s2) = decode_der_signature(&der).unwrap();
        assert_eq!(r, r2);
        assert_eq!(s, s2);
    }
}
