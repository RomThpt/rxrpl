use rxrpl_crypto::KeyType;

use super::base58;
use crate::error::CodecError;

/// Ed25519 seed prefix (3 bytes: starts with "sEd").
const ED25519_SEED_PREFIX: &[u8] = &[0x01, 0xE1, 0x4B];

/// Secp256k1 seed prefix (1 byte: family seed prefix 0x21).
const SECP256K1_SEED_PREFIX: &[u8] = &[0x21];

/// Encode a 16-byte seed entropy with the given key type.
pub fn encode_seed(entropy: &[u8; 16], key_type: KeyType) -> Result<String, CodecError> {
    let prefix = match key_type {
        KeyType::Ed25519 => ED25519_SEED_PREFIX,
        KeyType::Secp256k1 => SECP256K1_SEED_PREFIX,
    };
    Ok(base58::base58check_encode(entropy, prefix))
}

/// Decode a seed string, returning the 16-byte entropy and the key type.
pub fn decode_seed(seed: &str) -> Result<([u8; 16], KeyType), CodecError> {
    let decoded = base58::base58check_decode(seed)?;

    // Check for Ed25519 prefix (3 bytes)
    if decoded.len() >= 3 && decoded[..3] == *ED25519_SEED_PREFIX {
        let entropy = &decoded[3..];
        if entropy.len() != 16 {
            return Err(CodecError::InvalidSeed(format!(
                "expected 16 bytes entropy, got {}",
                entropy.len()
            )));
        }
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(entropy);
        return Ok((bytes, KeyType::Ed25519));
    }

    // Check for Secp256k1 prefix (1 byte)
    if !decoded.is_empty() && decoded[0] == SECP256K1_SEED_PREFIX[0] {
        let entropy = &decoded[1..];
        if entropy.len() != 16 {
            return Err(CodecError::InvalidSeed(format!(
                "expected 16 bytes entropy, got {}",
                entropy.len()
            )));
        }
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(entropy);
        return Ok((bytes, KeyType::Secp256k1));
    }

    Err(CodecError::InvalidSeed(
        "unrecognized seed prefix".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_ed25519_seed() {
        // Test vector from goXRPLd: "yurtyurtyurtyurt" -> "sEdTzRkEgPoxDG1mJ6WkSucHWnMkm1H"
        let entropy: [u8; 16] = *b"yurtyurtyurtyurt";
        let encoded = encode_seed(&entropy, KeyType::Ed25519).unwrap();
        assert_eq!(encoded, "sEdTzRkEgPoxDG1mJ6WkSucHWnMkm1H");

        let (decoded_entropy, key_type) = decode_seed(&encoded).unwrap();
        assert_eq!(decoded_entropy, entropy);
        assert_eq!(key_type, KeyType::Ed25519);
    }

    #[test]
    fn encode_decode_secp256k1_seed() {
        // Test vector from goXRPLd: "yurtyurtyurtyurt" -> "shPSkLzQNWfyXjZ7bbwgCky6twagA"
        let entropy: [u8; 16] = *b"yurtyurtyurtyurt";
        let encoded = encode_seed(&entropy, KeyType::Secp256k1).unwrap();
        assert_eq!(encoded, "shPSkLzQNWfyXjZ7bbwgCky6twagA");

        let (decoded_entropy, key_type) = decode_seed(&encoded).unwrap();
        assert_eq!(decoded_entropy, entropy);
        assert_eq!(key_type, KeyType::Secp256k1);
    }

    #[test]
    fn roundtrip_random_seed() {
        let seed = rxrpl_crypto::Seed::random();
        for key_type in [KeyType::Ed25519, KeyType::Secp256k1] {
            let encoded = encode_seed(seed.as_bytes(), key_type).unwrap();
            let (decoded, kt) = decode_seed(&encoded).unwrap();
            assert_eq!(&decoded, seed.as_bytes());
            assert_eq!(kt, key_type);
        }
    }
}
