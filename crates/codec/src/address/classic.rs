use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

use rxrpl_primitives::AccountId;

use super::base58;
use crate::error::CodecError;

/// XRPL type prefix for classic account addresses.
const ACCOUNT_ADDRESS_PREFIX: &[u8] = &[0x00];

/// Derive AccountId from a public key: SHA-256 then RIPEMD-160.
pub fn account_id_from_public_key(public_key: &[u8]) -> AccountId {
    let sha_hash = Sha256::digest(public_key);
    let ripe_hash = Ripemd160::digest(sha_hash);
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(&ripe_hash);
    AccountId(bytes)
}

/// Encode an AccountId to a classic address string (e.g., "rN7n...").
pub fn encode_account_id(account_id: &AccountId) -> String {
    base58::base58check_encode(account_id.as_bytes(), ACCOUNT_ADDRESS_PREFIX)
}

/// Decode a classic address string to an AccountId.
pub fn decode_account_id(address: &str) -> Result<AccountId, CodecError> {
    let decoded = base58::base58check_decode(address)?;
    if decoded.len() != 21 {
        return Err(CodecError::InvalidAddress(format!(
            "expected 21 bytes (1 prefix + 20 account id), got {}",
            decoded.len()
        )));
    }
    if decoded[0] != ACCOUNT_ADDRESS_PREFIX[0] {
        return Err(CodecError::InvalidAddress(
            "invalid address prefix".to_string(),
        ));
    }
    AccountId::from_slice(&decoded[1..21]).map_err(|e| CodecError::InvalidAddress(e.to_string()))
}

/// Encode a public key directly to a classic address.
pub fn encode_classic_address_from_pubkey(public_key: &[u8]) -> String {
    let account_id = account_id_from_public_key(public_key);
    encode_account_id(&account_id)
}

/// Check if a string is a valid classic address.
pub fn is_valid_classic_address(address: &str) -> bool {
    decode_account_id(address).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let account_id = AccountId::from_slice(
            &hex::decode("88a5a57c829f40f25ea83385bbde6c3d8b4ca082").unwrap(),
        )
        .unwrap();
        let address = encode_account_id(&account_id);
        let decoded = decode_account_id(&address).unwrap();
        assert_eq!(account_id, decoded);
    }

    #[test]
    fn classic_address_from_pubkey() {
        // Test vector from goXRPLd: ED9434799226374926EDA3B54B1B461B4ABF7237962EAE18528FEA67595397FA32
        // -> rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN
        let pubkey =
            hex::decode("ED9434799226374926EDA3B54B1B461B4ABF7237962EAE18528FEA67595397FA32")
                .unwrap();
        let address = encode_classic_address_from_pubkey(&pubkey);
        assert_eq!(address, "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN");

        // Verify the account_id is correct
        let account_id = account_id_from_public_key(&pubkey);
        assert_eq!(
            hex::encode(account_id.as_bytes()),
            "88a5a57c829f40f25ea83385bbde6c3d8b4ca082"
        );
    }

    #[test]
    fn invalid_address() {
        assert!(decode_account_id("invalid").is_err());
        assert!(decode_account_id("").is_err());
    }

    #[test]
    fn valid_address_check() {
        assert!(is_valid_classic_address(
            "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"
        ));
        assert!(!is_valid_classic_address("invalid"));
    }
}
