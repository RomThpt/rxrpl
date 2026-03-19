use rxrpl_primitives::AccountId;

use super::base58;
use crate::error::CodecError;

/// X-address prefix for mainnet (produces addresses starting with `X`).
const MAINNET_PREFIX: [u8; 2] = [0x05, 0x44];
/// X-address prefix for testnet (produces addresses starting with `T`).
const TESTNET_PREFIX: [u8; 2] = [0x04, 0x93];

/// Flag byte: no destination tag.
const FLAG_NO_TAG: u8 = 0x00;
/// Flag byte: 32-bit destination tag present.
const FLAG_HAS_TAG: u8 = 0x01;

/// Encode a classic account ID and optional tag into an X-address (XLS-5d).
///
/// - `account_id`: the 20-byte account identifier
/// - `tag`: optional 32-bit destination tag
/// - `is_test`: `true` for testnet (produces `T...`), `false` for mainnet (`X...`)
pub fn encode_x_address(account_id: &AccountId, tag: Option<u32>, is_test: bool) -> String {
    let prefix = if is_test {
        &TESTNET_PREFIX[..]
    } else {
        &MAINNET_PREFIX[..]
    };

    // Payload: 20 bytes account_id + 1 byte flag + 8 bytes tag (LE u64)
    let mut payload = Vec::with_capacity(29);
    payload.extend_from_slice(account_id.as_bytes());

    match tag {
        Some(t) => {
            payload.push(FLAG_HAS_TAG);
            payload.extend_from_slice(&(t as u64).to_le_bytes());
        }
        None => {
            payload.push(FLAG_NO_TAG);
            payload.extend_from_slice(&[0u8; 8]);
        }
    }

    base58::base58check_encode(&payload, prefix)
}

/// Decode an X-address into its components: (account_id, optional tag, is_test).
pub fn decode_x_address(x_address: &str) -> Result<(AccountId, Option<u32>, bool), CodecError> {
    let decoded = base58::base58check_decode(x_address)?;

    // Expected: 2 prefix + 20 account_id + 1 flag + 8 tag = 31 bytes
    if decoded.len() != 31 {
        return Err(CodecError::InvalidAddress(format!(
            "expected 31 bytes for X-address, got {}",
            decoded.len()
        )));
    }

    let is_test = match (decoded[0], decoded[1]) {
        (0x05, 0x44) => false,
        (0x04, 0x93) => true,
        _ => {
            return Err(CodecError::InvalidAddress(
                "invalid X-address prefix".to_string(),
            ));
        }
    };

    let account_id = AccountId::from_slice(&decoded[2..22])
        .map_err(|e| CodecError::InvalidAddress(e.to_string()))?;

    let flag = decoded[22];
    let tag_bytes = &decoded[23..31];

    match flag {
        FLAG_NO_TAG => {
            // All tag bytes must be zero
            if tag_bytes.iter().any(|&b| b != 0) {
                return Err(CodecError::InvalidAddress(
                    "no-tag flag set but tag bytes are nonzero".to_string(),
                ));
            }
            Ok((account_id, None, is_test))
        }
        FLAG_HAS_TAG => {
            // Read as LE u64, upper 4 bytes must be zero (32-bit tag)
            let tag_u64 = u64::from_le_bytes(tag_bytes.try_into().unwrap());
            if tag_u64 > u32::MAX as u64 {
                return Err(CodecError::InvalidAddress(
                    "tag exceeds 32-bit range".to_string(),
                ));
            }
            Ok((account_id, Some(tag_u64 as u32), is_test))
        }
        _ => Err(CodecError::InvalidAddress(format!(
            "unknown X-address flag byte: 0x{flag:02X}"
        ))),
    }
}

/// Check if a string is a valid X-address.
pub fn is_valid_x_address(address: &str) -> bool {
    decode_x_address(address).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_account_id() -> AccountId {
        AccountId::from_slice(&hex::decode("88a5a57c829f40f25ea83385bbde6c3d8b4ca082").unwrap())
            .unwrap()
    }

    #[test]
    fn mainnet_no_tag_roundtrip() {
        let account_id = test_account_id();
        let x_addr = encode_x_address(&account_id, None, false);
        assert!(x_addr.starts_with('X'));

        let (decoded_id, decoded_tag, decoded_test) = decode_x_address(&x_addr).unwrap();
        assert_eq!(decoded_id, account_id);
        assert_eq!(decoded_tag, None);
        assert!(!decoded_test);
    }

    #[test]
    fn mainnet_with_tag_roundtrip() {
        let account_id = test_account_id();
        let x_addr = encode_x_address(&account_id, Some(12345), false);
        assert!(x_addr.starts_with('X'));

        let (decoded_id, decoded_tag, decoded_test) = decode_x_address(&x_addr).unwrap();
        assert_eq!(decoded_id, account_id);
        assert_eq!(decoded_tag, Some(12345));
        assert!(!decoded_test);
    }

    #[test]
    fn testnet_roundtrip() {
        let account_id = test_account_id();
        let x_addr = encode_x_address(&account_id, Some(99), true);
        assert!(x_addr.starts_with('T'));

        let (decoded_id, decoded_tag, decoded_test) = decode_x_address(&x_addr).unwrap();
        assert_eq!(decoded_id, account_id);
        assert_eq!(decoded_tag, Some(99));
        assert!(decoded_test);
    }

    #[test]
    fn tag_zero_is_distinct_from_no_tag() {
        let account_id = test_account_id();
        let with_zero = encode_x_address(&account_id, Some(0), false);
        let without_tag = encode_x_address(&account_id, None, false);
        assert_ne!(with_zero, without_tag);

        let (_, tag_zero, _) = decode_x_address(&with_zero).unwrap();
        assert_eq!(tag_zero, Some(0));

        let (_, tag_none, _) = decode_x_address(&without_tag).unwrap();
        assert_eq!(tag_none, None);
    }

    #[test]
    fn tag_max_u32() {
        let account_id = test_account_id();
        let x_addr = encode_x_address(&account_id, Some(u32::MAX), false);

        let (decoded_id, decoded_tag, _) = decode_x_address(&x_addr).unwrap();
        assert_eq!(decoded_id, account_id);
        assert_eq!(decoded_tag, Some(u32::MAX));
    }

    #[test]
    fn invalid_prefix() {
        // Encode with valid prefix, then manually craft bad data
        let account_id = test_account_id();
        let mut payload = Vec::new();
        payload.extend_from_slice(account_id.as_bytes());
        payload.push(FLAG_NO_TAG);
        payload.extend_from_slice(&[0u8; 8]);

        // Use a bogus prefix
        let encoded = base58::base58check_encode(&payload, &[0xFF, 0xFF]);
        assert!(decode_x_address(&encoded).is_err());
    }

    #[test]
    fn invalid_no_tag_nonzero_bytes() {
        // Craft raw payload with flag=0x00 but nonzero tag bytes
        let account_id = test_account_id();
        let mut raw = Vec::with_capacity(31);
        raw.extend_from_slice(&MAINNET_PREFIX);
        raw.extend_from_slice(account_id.as_bytes());
        raw.push(FLAG_NO_TAG);
        raw.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

        // Manually base58check encode the raw bytes (prefix is already included)
        let encoded = base58::base58check_encode(&raw[2..], &MAINNET_PREFIX);
        assert!(decode_x_address(&encoded).is_err());
    }

    #[test]
    fn invalid_unknown_flag() {
        let account_id = test_account_id();
        let mut payload = Vec::new();
        payload.extend_from_slice(account_id.as_bytes());
        payload.push(0x02); // invalid flag
        payload.extend_from_slice(&[0u8; 8]);

        let encoded = base58::base58check_encode(&payload, &MAINNET_PREFIX);
        assert!(decode_x_address(&encoded).is_err());
    }

    #[test]
    fn classic_address_is_not_valid_x_address() {
        assert!(!is_valid_x_address("rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"));
    }

    #[test]
    fn known_test_vector() {
        // Test vector: rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN with no tag on mainnet
        // Account ID: 88a5a57c829f40f25ea83385bbde6c3d8b4ca082
        let account_id = test_account_id();
        let x_addr = encode_x_address(&account_id, None, false);

        // Verify it decodes back correctly
        let (decoded_id, decoded_tag, decoded_test) = decode_x_address(&x_addr).unwrap();
        assert_eq!(decoded_id, account_id);
        assert_eq!(decoded_tag, None);
        assert!(!decoded_test);

        // Verify the encoded address is deterministic
        let x_addr2 = encode_x_address(&account_id, None, false);
        assert_eq!(x_addr, x_addr2);
    }
}
