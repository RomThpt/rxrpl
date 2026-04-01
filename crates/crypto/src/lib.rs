//! Cryptographic operations for the XRP Ledger.
//!
//! Supports Ed25519 and secp256k1 key derivation, signing, and verification.
//! Includes seed generation, DER encoding, and XRPL-specific hash prefixes.

pub mod der;
pub mod ed25519;
pub mod hash_prefix;
pub mod key_pair;
pub mod key_type;
pub mod multisign;
pub mod secp256k1;
pub mod seed;
pub mod sha512_half;
pub mod sign;

pub use key_pair::KeyPair;
pub use key_type::KeyType;
pub use seed::Seed;

/// Verify a signature against a message and public key.
///
/// Automatically dispatches to Ed25519 or secp256k1 based on the
/// public key prefix (0xED = Ed25519, otherwise secp256k1).
pub fn verify_signature(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    if public_key.is_empty() {
        return false;
    }
    if public_key[0] == 0xED {
        ed25519::verify(message, public_key, signature)
    } else {
        secp256k1::verify(message, public_key, signature)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid private key")]
    InvalidPrivateKey,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("signing failed")]
    SigningFailed,
    #[error("invalid signature")]
    InvalidSignature,
}
