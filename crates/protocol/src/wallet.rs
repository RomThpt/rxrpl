use rxrpl_codec::address::{decode_seed, encode_classic_address_from_pubkey, encode_seed};
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_primitives::{Hash256, PublicKey};
use serde_json::Value;

use crate::error::ProtocolError;
use crate::tx;
use crate::tx::common::Signer;

/// XRPL wallet encapsulating seed, keypair, and address.
///
/// Provides a clean API for signing transactions without
/// manually juggling seeds, keypairs, and address derivation.
pub struct Wallet {
    pub address: String,
    pub public_key: PublicKey,
    pub key_type: KeyType,
    seed: Seed,
    key_pair: KeyPair,
}

impl Wallet {
    /// Generate a new random wallet.
    pub fn generate(key_type: KeyType) -> Self {
        let seed = Seed::random();
        Self::build(seed, key_type)
    }

    /// Create a wallet from an encoded seed string (sXXX format).
    ///
    /// Auto-detects key type from the base58 prefix:
    /// - `sEd...` seeds decode as Ed25519
    /// - Other seeds decode as Secp256k1
    pub fn from_seed(seed_str: &str) -> Result<Self, ProtocolError> {
        let (entropy, key_type) = decode_seed(seed_str)?;
        Ok(Self::build(Seed::from_bytes(entropy), key_type))
    }

    /// Create a wallet from an encoded seed with an explicit key type override.
    pub fn from_seed_with_type(seed_str: &str, key_type: KeyType) -> Result<Self, ProtocolError> {
        let (entropy, _) = decode_seed(seed_str)?;
        Ok(Self::build(Seed::from_bytes(entropy), key_type))
    }

    /// Create a wallet from raw 16-byte entropy.
    pub fn from_entropy(entropy: [u8; 16], key_type: KeyType) -> Self {
        Self::build(Seed::from_bytes(entropy), key_type)
    }

    /// Return the base58-encoded seed string (sXXX format).
    pub fn seed_encoded(&self) -> Result<String, ProtocolError> {
        encode_seed(self.seed.as_bytes(), self.key_type).map_err(ProtocolError::Codec)
    }

    /// Sign a transaction JSON using this wallet's keypair.
    ///
    /// Returns a new JSON value with `SigningPubKey` and `TxnSignature` populated.
    pub fn sign(&self, tx_json: &Value) -> Result<Value, ProtocolError> {
        tx::sign(tx_json, &self.key_pair)
    }

    /// Sign a transaction, serialize to hex blob, and compute the hash.
    ///
    /// Returns `(tx_blob, tx_hash)`.
    pub fn sign_and_serialize(&self, tx_json: &Value) -> Result<(String, Hash256), ProtocolError> {
        let signed = self.sign(tx_json)?;
        let blob = tx::serialize_signed(&signed)?;
        let hash = tx::compute_tx_hash(&signed)?;
        Ok((blob, hash))
    }

    /// Produce a `Signer` entry for multi-signing, using a specific signer account.
    pub fn sign_for(&self, tx_json: &Value, signer_account: &str) -> Result<Signer, ProtocolError> {
        tx::sign_for(tx_json, &self.key_pair, signer_account)
    }

    /// Produce a `Signer` entry for multi-signing, using this wallet's own address.
    pub fn multisign(&self, tx_json: &Value) -> Result<Signer, ProtocolError> {
        tx::sign_for(tx_json, &self.key_pair, &self.address)
    }

    fn build(seed: Seed, key_type: KeyType) -> Self {
        let key_pair = KeyPair::from_seed(&seed, key_type);
        let address = encode_classic_address_from_pubkey(key_pair.public_key.as_bytes());
        let public_key = key_pair.public_key.clone();
        Self {
            address,
            public_key,
            key_type,
            seed,
            key_pair,
        }
    }
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wallet")
            .field("address", &self.address)
            .field("public_key", &hex::encode(self.public_key.as_bytes()))
            .field("key_type", &self.key_type)
            .field("seed", &"REDACTED")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_ed25519() {
        let w = Wallet::generate(KeyType::Ed25519);
        assert!(w.address.starts_with('r'));
        assert_eq!(w.key_type, KeyType::Ed25519);
        assert_eq!(w.public_key.as_bytes().len(), 33);
        assert_eq!(w.public_key.as_bytes()[0], 0xED);
    }

    #[test]
    fn generate_secp256k1() {
        let w = Wallet::generate(KeyType::Secp256k1);
        assert!(w.address.starts_with('r'));
        assert_eq!(w.key_type, KeyType::Secp256k1);
        assert_eq!(w.public_key.as_bytes().len(), 33);
        assert!(w.public_key.as_bytes()[0] == 0x02 || w.public_key.as_bytes()[0] == 0x03);
    }

    #[test]
    fn from_seed_roundtrip() {
        let w1 = Wallet::generate(KeyType::Ed25519);
        let encoded = w1.seed_encoded().unwrap();
        let w2 = Wallet::from_seed(&encoded).unwrap();
        assert_eq!(w1.address, w2.address);
        assert_eq!(w1.key_type, w2.key_type);
    }

    #[test]
    fn from_seed_auto_detects_key_type() {
        let w = Wallet::generate(KeyType::Ed25519);
        let encoded = w.seed_encoded().unwrap();
        assert!(encoded.starts_with("sEd"));
        let w2 = Wallet::from_seed(&encoded).unwrap();
        assert_eq!(w2.key_type, KeyType::Ed25519);
    }

    #[test]
    fn from_entropy_deterministic() {
        let entropy = [1u8; 16];
        let w1 = Wallet::from_entropy(entropy, KeyType::Ed25519);
        let w2 = Wallet::from_entropy(entropy, KeyType::Ed25519);
        assert_eq!(w1.address, w2.address);
    }

    #[test]
    fn from_seed_with_type_override() {
        let entropy = [2u8; 16];
        let w_ed = Wallet::from_entropy(entropy, KeyType::Ed25519);
        let w_secp = Wallet::from_entropy(entropy, KeyType::Secp256k1);
        assert_ne!(w_ed.address, w_secp.address);
    }

    #[test]
    fn debug_redacts_seed() {
        let w = Wallet::generate(KeyType::Ed25519);
        let dbg = format!("{:?}", w);
        assert!(dbg.contains("REDACTED"));
        // The seed_encoded value should NOT appear in debug output
        let encoded = w.seed_encoded().unwrap();
        assert!(!dbg.contains(&encoded));
    }

    #[test]
    fn sign_produces_valid_json() {
        let w = Wallet::from_entropy([3u8; 16], KeyType::Ed25519);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": w.address,
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        });
        let signed = w.sign(&tx).unwrap();
        assert!(signed["SigningPubKey"].is_string());
        assert!(signed["TxnSignature"].is_string());
    }

    #[test]
    fn wallet_multisign_produces_valid_signer() {
        let w = Wallet::from_entropy([10u8; 16], KeyType::Ed25519);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        });
        let signer = w.multisign(&tx).unwrap();
        assert_eq!(signer.signer.account, w.address);
        assert!(!signer.signer.txn_signature.is_empty());
        assert!(!signer.signer.signing_pub_key.is_empty());
    }

    #[test]
    fn wallet_multisign_full_workflow() {
        use crate::tx::{combine_multisig, verify_multisig};

        let w1 = Wallet::from_entropy([20u8; 16], KeyType::Ed25519);
        let w2 = Wallet::from_entropy([21u8; 16], KeyType::Secp256k1);
        let w3 = Wallet::from_entropy([22u8; 16], KeyType::Ed25519);

        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "500000",
            "Fee": "12",
            "Sequence": 5
        });

        let s1 = w1.multisign(&tx).unwrap();
        let s2 = w2.multisign(&tx).unwrap();
        let s3 = w3.multisign(&tx).unwrap();

        let combined = combine_multisig(&tx, vec![s1, s2, s3]).unwrap();
        verify_multisig(&combined).unwrap();
    }

    #[test]
    fn sign_and_serialize_returns_blob_and_hash() {
        let w = Wallet::from_entropy([4u8; 16], KeyType::Ed25519);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": w.address,
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        });
        let (blob, hash) = w.sign_and_serialize(&tx).unwrap();
        assert!(!blob.is_empty());
        assert!(hex::decode(&blob).is_ok());
        assert!(!hash.is_zero());
        assert_eq!(hash.to_string().len(), 64);
    }
}
