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
