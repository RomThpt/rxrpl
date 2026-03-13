use sha2::{Digest, Sha256};

use crate::error::CodecError;

/// XRPL-specific Base58 alphabet.
const XRPL_ALPHABET: &[u8; 58] = b"rpshnaf39wBUDNEGHJKLM4PQRST7VWXYZ2bcdeCg65jkm8oFqi1tuvAxyz";

/// Reverse lookup table: byte value -> index in alphabet (255 = invalid)
const fn build_decode_table() -> [u8; 128] {
    let mut table = [255u8; 128];
    let mut i = 0;
    while i < 58 {
        table[XRPL_ALPHABET[i] as usize] = i as u8;
        i += 1;
    }
    table
}

const DECODE_TABLE: [u8; 128] = build_decode_table();

/// Double-SHA-256 checksum (first 4 bytes).
fn checksum(data: &[u8]) -> [u8; 4] {
    let hash1 = Sha256::digest(data);
    let hash2 = Sha256::digest(hash1);
    let mut out = [0u8; 4];
    out.copy_from_slice(&hash2[..4]);
    out
}

/// Encode bytes with a prefix using Base58Check (prefix + payload + 4-byte checksum).
pub fn base58check_encode(payload: &[u8], prefix: &[u8]) -> String {
    let mut data = Vec::with_capacity(prefix.len() + payload.len() + 4);
    data.extend_from_slice(prefix);
    data.extend_from_slice(payload);
    let check = checksum(&data);
    data.extend_from_slice(&check);
    base58_encode(&data)
}

/// Decode a Base58Check string, verify the checksum, return prefix + payload (without checksum).
pub fn base58check_decode(encoded: &str) -> Result<Vec<u8>, CodecError> {
    let data = base58_decode(encoded)?;
    if data.len() < 5 {
        return Err(CodecError::InvalidChecksum);
    }
    let (payload, check) = data.split_at(data.len() - 4);
    let expected = checksum(payload);
    if check != expected {
        return Err(CodecError::InvalidChecksum);
    }
    Ok(payload.to_vec())
}

/// Raw Base58 encoding (no checksum).
fn base58_encode(data: &[u8]) -> String {
    // Count leading zeros
    let leading_zeros = data.iter().take_while(|&&b| b == 0).count();

    // Convert to base58 using bignum division
    let mut num = data.to_vec();
    let mut result = Vec::new();

    while !num.is_empty() {
        let mut remainder = 0u32;
        let mut new_num = Vec::new();
        for &byte in &num {
            let acc = (remainder << 8) | byte as u32;
            let quotient = acc / 58;
            remainder = acc % 58;
            if !new_num.is_empty() || quotient > 0 {
                new_num.push(quotient as u8);
            }
        }
        result.push(XRPL_ALPHABET[remainder as usize]);
        num = new_num;
    }

    // Add leading 'r' characters for leading zeros
    for _ in 0..leading_zeros {
        result.push(XRPL_ALPHABET[0]);
    }

    result.reverse();
    String::from_utf8(result).unwrap()
}

/// Raw Base58 decoding (no checksum verification).
fn base58_decode(encoded: &str) -> Result<Vec<u8>, CodecError> {
    let leading_ones = encoded
        .bytes()
        .take_while(|&b| b == XRPL_ALPHABET[0])
        .count();

    let mut num: Vec<u8> = Vec::new();

    for byte in encoded.bytes() {
        if byte > 127 {
            return Err(CodecError::InvalidBase58);
        }
        let value = DECODE_TABLE[byte as usize];
        if value == 255 {
            return Err(CodecError::InvalidBase58);
        }

        // Multiply num by 58 and add value
        let mut carry = value as u32;
        for digit in num.iter_mut().rev() {
            let acc = (*digit as u32) * 58 + carry;
            *digit = (acc & 0xFF) as u8;
            carry = acc >> 8;
        }
        while carry > 0 {
            num.insert(0, (carry & 0xFF) as u8);
            carry >>= 8;
        }
    }

    // Add leading zeros
    let mut result = vec![0u8; leading_ones];
    result.extend_from_slice(&num);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let data = b"hello";
        let prefix = &[0x00];
        let encoded = base58check_encode(data, prefix);
        let decoded = base58check_decode(&encoded).unwrap();
        assert_eq!(&decoded[0..1], prefix);
        assert_eq!(&decoded[1..], data);
    }

    #[test]
    fn invalid_checksum() {
        let encoded = base58check_encode(b"test", &[0x00]);
        // Corrupt last character
        let mut chars: Vec<char> = encoded.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'r' { 'p' } else { 'r' };
        let corrupted: String = chars.into_iter().collect();
        assert!(base58check_decode(&corrupted).is_err());
    }
}
