use rxrpl_primitives::{PublicKey, Signature};

use crate::CryptoError;

/// Trait for signing data.
pub trait SignerTrait {
    fn sign(&self, message: &[u8]) -> Result<Signature, CryptoError>;
    fn public_key(&self) -> &PublicKey;
}

/// Trait for verifying signatures.
pub trait VerifierTrait {
    fn verify(&self, message: &[u8], signature: &Signature) -> bool;
}
