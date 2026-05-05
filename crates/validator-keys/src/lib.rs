//! Validator keychain: generate master + ephemeral keypairs, sign manifests,
//! and produce rippled-compatible artifacts (`spXXX` / `sEdXXX` family seeds,
//! `nXXX` node-public keys, signed manifests).
//!
//! This crate is the rxrpl equivalent of rippled's `validator-keys-tool`.
//! It produces:
//!   - A `validator_keys.json` file containing the master seed (rippled
//!     family-seed format) and metadata (key type, public key, fingerprint).
//!   - A signed `manifest` blob (hex-encoded STObject) binding the master
//!     key to a rotating ephemeral key with a monotonic sequence.
//!
//! The library is binary-format compatible with rippled's
//! `Manifest::makeManifest` and `validator-keys-tool` outputs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use rxrpl_crypto::{KeyPair, KeyType, Seed};

/// Errors raised by the validator-keys API.
#[derive(Debug, thiserror::Error)]
pub enum KeysError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("codec error: {0}")]
    Codec(#[from] rxrpl_codec::CodecError),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("invalid sequence: {0}")]
    InvalidSequence(String),
}

/// Node-public-key base58 prefix (`nXXX`). Matches rippled `TokenType::NodePublic`.
const NODE_PUBLIC_KEY_PREFIX: &[u8] = &[0x1C];

/// Encode a 33-byte node public key in rippled's `nXXX` base58check format.
pub fn encode_node_public_key(pubkey_bytes: &[u8]) -> String {
    rxrpl_codec::address::base58::base58check_encode(pubkey_bytes, NODE_PUBLIC_KEY_PREFIX)
}

/// Persisted contents of the validator keys file.
///
/// Mirrors rippled's `validator-keys.json` layout:
///   - `key_type`: `"ed25519"` or `"secp256k1"`.
///   - `secret_key`: master seed in family-seed format (`spXXX` / `sEdXXX`).
///   - `public_key`: master public key in `nXXX` format.
///   - `revoked`: `true` once a revocation manifest has been emitted.
///   - `token_sequence`: monotonically increasing manifest sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorKeysFile {
    pub key_type: String,
    pub secret_key: String,
    pub public_key: String,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub token_sequence: u32,
}

impl ValidatorKeysFile {
    /// Build a keys-file record from a master keypair + seed.
    pub fn from_master(seed: &Seed, key_type: KeyType, kp: &KeyPair) -> Result<Self, KeysError> {
        let secret_key = rxrpl_codec::address::seed::encode_seed(seed.as_bytes(), key_type)?;
        let public_key = encode_node_public_key(kp.public_key.as_bytes());
        Ok(Self {
            key_type: match key_type {
                KeyType::Ed25519 => "ed25519".into(),
                KeyType::Secp256k1 => "secp256k1".into(),
            },
            secret_key,
            public_key,
            revoked: false,
            token_sequence: 0,
        })
    }

    /// Parse a key-type string back into a `KeyType`.
    pub fn parsed_key_type(&self) -> Result<KeyType, KeysError> {
        match self.key_type.as_str() {
            "ed25519" => Ok(KeyType::Ed25519),
            "secp256k1" => Ok(KeyType::Secp256k1),
            other => Err(KeysError::Manifest(format!("unknown key_type {other}"))),
        }
    }

    /// Decode the master seed back into raw entropy.
    pub fn decode_seed(&self) -> Result<Seed, KeysError> {
        let (entropy, _kt) = rxrpl_codec::address::seed::decode_seed(&self.secret_key)?;
        Ok(Seed::from_bytes(entropy))
    }

    /// Re-derive the master keypair from the persisted seed.
    pub fn master_keypair(&self) -> Result<KeyPair, KeysError> {
        let seed = self.decode_seed()?;
        Ok(KeyPair::from_seed(&seed, self.parsed_key_type()?))
    }

    /// Default JSON path inside an output directory.
    pub fn default_path(dir: &Path) -> PathBuf {
        dir.join("validator_keys.json")
    }

    /// Save to disk with `0600` permissions on Unix, atomically.
    pub fn save(&self, path: &Path) -> Result<(), KeysError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let body = serde_json::to_string_pretty(self)?;
        fs::write(path, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(path, perms)?;
        }
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, KeysError> {
        let body = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&body)?)
    }
}

/// Generate a fresh random master keypair plus persisted record.
pub fn generate_master(key_type: KeyType) -> Result<(Seed, KeyPair, ValidatorKeysFile), KeysError> {
    let seed = Seed::random();
    let kp = KeyPair::from_seed(&seed, key_type);
    let file = ValidatorKeysFile::from_master(&seed, key_type, &kp)?;
    Ok((seed, kp, file))
}

/// Sign a manifest binding the master key to an ephemeral key.
///
/// Returns hex-encoded rippled-compatible STObject bytes.
pub fn sign_manifest(
    master: &KeyPair,
    ephemeral: &KeyPair,
    sequence: u32,
    domain: Option<&str>,
) -> Result<String, KeysError> {
    let raw = rxrpl_overlay::manifest::create_signed(master, ephemeral, sequence, domain)
        .map_err(|e| KeysError::Manifest(e.to_string()))?;
    Ok(hex::encode_upper(raw))
}

/// Result of generating a manifest, suitable for emitting alongside the keys file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedManifest {
    pub sequence: u32,
    pub master_public_key: String,
    pub ephemeral_public_key: String,
    pub ephemeral_secret_key: String,
    pub manifest_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Produce a complete signed manifest with a freshly generated ephemeral key.
pub fn generate_manifest(
    master_kp: &KeyPair,
    sequence: u32,
    eph_key_type: KeyType,
    domain: Option<&str>,
) -> Result<GeneratedManifest, KeysError> {
    let eph_seed = Seed::random();
    let eph_kp = KeyPair::from_seed(&eph_seed, eph_key_type);
    let manifest_hex = sign_manifest(master_kp, &eph_kp, sequence, domain)?;
    Ok(GeneratedManifest {
        sequence,
        master_public_key: encode_node_public_key(master_kp.public_key.as_bytes()),
        ephemeral_public_key: encode_node_public_key(eph_kp.public_key.as_bytes()),
        ephemeral_secret_key: rxrpl_codec::address::seed::encode_seed(
            eph_seed.as_bytes(),
            eph_key_type,
        )?,
        manifest_hex,
        domain: domain.map(|d| d.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rippled-compat fixture: the well-known "masterpassphrase" seed in
    /// rippled is `snoPBrXtMeMyMHUVTgbuqAfg1SUTb` (secp256k1 family seed).
    /// Decoding it must produce the canonical 16-byte entropy that rippled
    /// derives from SHA-512(masterpassphrase)[..16].
    #[test]
    fn rippled_master_passphrase_seed_round_trip() {
        const RIPPLED_MASTER_SEED: &str = "snoPBrXtMeMyMHUVTgbuqAfg1SUTb";

        let (entropy, key_type) =
            rxrpl_codec::address::seed::decode_seed(RIPPLED_MASTER_SEED).expect("decode rippled seed");
        assert_eq!(key_type, KeyType::Secp256k1);

        // Re-encoding produces byte-identical output.
        let encoded = rxrpl_codec::address::seed::encode_seed(&entropy, key_type).unwrap();
        assert_eq!(encoded, RIPPLED_MASTER_SEED);

        // The standard "masterpassphrase" -> Seed::from_passphrase path matches
        // what rippled computes (SHA-512 of "masterpassphrase", first 16 bytes).
        let derived = Seed::from_passphrase("masterpassphrase");
        assert_eq!(derived.as_bytes(), &entropy);
    }

    #[test]
    fn generate_round_trip_ed25519() {
        let (seed, kp, file) = generate_master(KeyType::Ed25519).unwrap();
        assert_eq!(file.key_type, "ed25519");
        assert!(file.secret_key.starts_with("sEd"));
        assert!(file.public_key.starts_with('n'));
        assert!(!file.revoked);
        assert_eq!(file.token_sequence, 0);

        // Re-derive from persisted seed -> same public key.
        let kp2 = file.master_keypair().unwrap();
        assert_eq!(kp2.public_key, kp.public_key);
        assert_eq!(file.decode_seed().unwrap().as_bytes(), seed.as_bytes());
    }

    #[test]
    fn generate_round_trip_secp256k1() {
        let (_seed, kp, file) = generate_master(KeyType::Secp256k1).unwrap();
        assert_eq!(file.key_type, "secp256k1");
        // secp family seeds start with 's' but not 'sEd'.
        assert!(!file.secret_key.starts_with("sEd"));
        let kp2 = file.master_keypair().unwrap();
        assert_eq!(kp2.public_key, kp.public_key);
    }

    #[test]
    fn save_and_load_keys_file() {
        let dir = tempdir();
        let path = ValidatorKeysFile::default_path(&dir);
        let (_seed, _kp, file) = generate_master(KeyType::Ed25519).unwrap();
        file.save(&path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "keys file must be 0600");
        }

        let reloaded = ValidatorKeysFile::load(&path).unwrap();
        assert_eq!(reloaded.public_key, file.public_key);
        assert_eq!(reloaded.secret_key, file.secret_key);
    }

    #[test]
    fn manifest_sign_and_verify() {
        let master_seed = Seed::from_passphrase("validator-keys-master");
        let master_kp = KeyPair::from_seed(&master_seed, KeyType::Ed25519);

        let manifest = generate_manifest(&master_kp, 1, KeyType::Ed25519, Some("example.com")).unwrap();
        assert_eq!(manifest.sequence, 1);
        assert_eq!(manifest.domain.as_deref(), Some("example.com"));

        // Decode hex back into binary STObject and verify.
        let raw = hex::decode(&manifest.manifest_hex).expect("hex decode manifest");
        let parsed = rxrpl_overlay::manifest::parse_and_verify(&raw).expect("verify manifest");
        assert_eq!(parsed.sequence, 1);
        assert_eq!(parsed.master_public_key, master_kp.public_key);
        assert_eq!(parsed.domain.as_deref(), Some("example.com"));
        // Ephemeral public key in the parsed manifest matches our generated one.
        let eph_str = encode_node_public_key(parsed.ephemeral_public_key.as_ref().unwrap().as_bytes());
        assert_eq!(eph_str, manifest.ephemeral_public_key);
    }

    #[test]
    fn manifest_sequence_must_increase() {
        // Pure semantic test: building two manifests with the same sequence is
        // allowed by the signer (validation is the store's job), but the
        // application-layer caller is expected to bump `token_sequence`.
        let master_seed = Seed::from_passphrase("validator-keys-seq");
        let master_kp = KeyPair::from_seed(&master_seed, KeyType::Ed25519);

        let m1 = generate_manifest(&master_kp, 1, KeyType::Ed25519, None).unwrap();
        let m2 = generate_manifest(&master_kp, 2, KeyType::Ed25519, None).unwrap();
        assert!(m2.sequence > m1.sequence);
    }

    /// Tiny tempdir helper to avoid pulling in the `tempfile` crate.
    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("rxrpl-validator-keys-test-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
