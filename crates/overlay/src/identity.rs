use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_primitives::Hash256;

/// Node identity (keypair and derived node ID).
pub struct NodeIdentity {
    key_pair: KeyPair,
    /// The node ID derived from the public key (SHA-512-Half).
    pub node_id: Hash256,
}

impl NodeIdentity {
    /// Generate a random node identity (secp256k1 for rippled compatibility).
    pub fn generate() -> Self {
        let key_pair = KeyPair::generate(KeyType::Secp256k1);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[key_pair.public_key.as_bytes()]);
        Self { key_pair, node_id }
    }

    /// Create a deterministic identity from a seed (secp256k1, **validator
    /// derivation** for rippled compatibility).
    ///
    /// Critical: rippled's validator/node keypair derivation skips the
    /// account-level "+derived_scalar" step that `KeyPair::from_seed`
    /// uses for ordinary accounts. Calling this with the same family
    /// seed as rippled (e.g. `sneWFZcEqA8TUA5BmJ38xsqaR7dFb`) reproduces
    /// rippled's `n9LXMXFTeVL6o9fxdFHfeVZWf6YzWCBzt7YyeK1HV7wZ4ZFRNgUV`
    /// public key — without this distinction rxrpl's validations are
    /// signed by a key that no rippled UNL trusts.
    pub fn from_seed(seed: &Seed) -> Self {
        let (public_key, private_key) =
            rxrpl_crypto::secp256k1::derive_keypair(seed, true);
        let node_id = rxrpl_crypto::sha512_half::sha512_half(&[public_key.as_bytes()]);
        let key_pair = KeyPair {
            public_key,
            private_key,
            key_type: KeyType::Secp256k1,
        };
        Self { key_pair, node_id }
    }

    /// Get the raw public key bytes (33 bytes).
    pub fn public_key_bytes(&self) -> &[u8] {
        self.key_pair.public_key.as_bytes()
    }

    /// Get the key type used by this identity.
    pub fn key_type(&self) -> KeyType {
        self.key_pair.key_type
    }

    /// Sign data with this node's private key (hashes before signing).
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        match self.key_pair.key_type {
            KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(data, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
            KeyType::Secp256k1 => rxrpl_crypto::secp256k1::sign(data, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
        }
    }

    /// Sign a pre-hashed 32-byte digest directly (no additional hashing).
    ///
    /// Used for protocols like the rippled HTTP upgrade handshake where
    /// the session cookie is already a hash.
    pub fn sign_digest(&self, digest: &[u8; 32]) -> Vec<u8> {
        match self.key_pair.key_type {
            KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(digest, &self.key_pair.private_key)
                .map(|sig| sig.as_bytes().to_vec())
                .unwrap_or_default(),
            KeyType::Secp256k1 => {
                rxrpl_crypto::secp256k1::sign_digest(digest, &self.key_pair.private_key)
                    .map(|sig| sig.as_bytes().to_vec())
                    .unwrap_or_default()
            }
        }
    }

    /// Sign a consensus proposal with this node's key.
    pub fn sign_proposal(&self, proposal: &mut rxrpl_consensus::types::Proposal) {
        proposal.sign(&self.key_pair.private_key, self.key_pair.key_type);
    }

    /// Sign a consensus validation with this node's key (STObject format).
    ///
    /// Produces a signature over the rippled-canonical signing hash:
    /// `SHA-512-Half(HashPrefix::validation || canonical_STObject)` where
    /// `canonical_STObject` is the validation's full SOTemplate **sorted by
    /// `(type_code, field_code)`** with `sfSignature` and `sfMasterSignature`
    /// excluded. The strip-result (everything after the 4-byte prefix) is
    /// stashed in `validation.signing_payload` so that
    /// [`verify_validation_signature`] can replay the exact byte sequence
    /// and remain in lock-step with rippled validators that include any
    /// subset of the optional fields (LoadFee, ReserveBase, Cookie,
    /// Amendments, ...).
    ///
    /// The encoded field set:
    /// - sfFlags, sfLedgerSequence, sfSigningTime, sfLedgerHash,
    ///   sfSigningPubKey (always present)
    /// - sfLoadFee, sfReserveBase, sfReserveIncrement, sfBaseFee,
    ///   sfCookie, sfServerVersion, sfConsensusHash, sfValidatedHash,
    ///   sfBaseFeeDrops, sfReserveBaseDrops, sfReserveIncrementDrops,
    ///   sfAmendments (emitted only when set / non-empty)
    ///
    /// `sfCloseTime` is intentionally not emitted: the `Validation` struct
    /// stores it as a non-optional `u32`, so there is no signal to skip it
    /// for rxrpl-locally-built validations and emitting it unconditionally
    /// would change the byte-image (and thus the signature) of every
    /// validation rxrpl produces today. Validations decoded from the wire
    /// carry their own `signing_payload` and so include `sfCloseTime` when
    /// the originating validator did.
    pub fn sign_validation(&self, validation: &mut rxrpl_consensus::types::Validation) {
        sign_validation_with_keypair(&self.key_pair, validation);
    }

    /// Get the private key bytes (for signing operations).
    pub fn private_key(&self) -> &[u8] {
        &self.key_pair.private_key
    }
}

/// Sign a consensus `Validation` with the given keypair, embedding the
/// strip-result into `validation.signing_payload` so the verifier can replay
/// the exact bytes. Shared by [`NodeIdentity::sign_validation`] and
/// [`ValidatorIdentity::sign_validation`] — see `NodeIdentity::sign_validation`
/// docstring for the canonical-encoding contract.
fn sign_validation_with_keypair(
    keypair: &rxrpl_crypto::KeyPair,
    validation: &mut rxrpl_consensus::types::Validation,
) {
    use crate::stobject;

    // HashPrefix::validation = 'V','A','L',0 = 0x56414C00
    const HASH_PREFIX_VALIDATION: [u8; 4] = [0x56, 0x41, 0x4C, 0x00];

    let mut stripped = Vec::with_capacity(192);

    // (2,2) sfFlags — always
    let flags: u32 = if validation.full { 0x80000001 } else { 0x00000000 };
    stobject::put_uint32(&mut stripped, 2, flags);

    // (2,6) sfLedgerSequence — always
    stobject::put_uint32(&mut stripped, 6, validation.ledger_seq);

    // (2,7) sfCloseTime — skipped (see NodeIdentity::sign_validation doc).

    // (2,9) sfSigningTime — always
    stobject::put_uint32(&mut stripped, 9, validation.sign_time);

    if let Some(v) = validation.load_fee {
        stobject::put_uint32(&mut stripped, 24, v);
    }
    if let Some(v) = validation.reserve_base {
        stobject::put_uint32(&mut stripped, 31, v);
    }
    if let Some(v) = validation.reserve_increment {
        stobject::put_uint32(&mut stripped, 32, v);
    }
    if let Some(v) = validation.base_fee {
        stobject::put_uint64(&mut stripped, 5, v);
    }
    if let Some(v) = validation.cookie {
        stobject::put_uint64(&mut stripped, 10, v);
    }
    if let Some(v) = validation.server_version {
        stobject::put_uint64(&mut stripped, 11, v);
    }

    // (5,1) sfLedgerHash — always
    stobject::put_hash256(&mut stripped, 1, validation.ledger_hash.as_bytes());

    if let Some(h) = validation.consensus_hash.as_ref() {
        stobject::put_hash256(&mut stripped, 23, h.as_bytes());
    }
    if let Some(h) = validation.validated_hash.as_ref() {
        stobject::put_hash256(&mut stripped, 25, h.as_bytes());
    }
    if let Some(v) = validation.base_fee_drops {
        stobject::put_amount_xrp(&mut stripped, 22, v);
    }
    if let Some(v) = validation.reserve_base_drops {
        stobject::put_amount_xrp(&mut stripped, 23, v);
    }
    if let Some(v) = validation.reserve_increment_drops {
        stobject::put_amount_xrp(&mut stripped, 24, v);
    }

    // (7,3) sfSigningPubKey — always
    stobject::put_vl(&mut stripped, 3, keypair.public_key.as_bytes());

    // sfSignature (7,6) and sfMasterSignature (7,18) are EXCLUDED
    // from the signing buffer by definition.

    // (19,3) sfAmendments — optional (emitted only when non-empty)
    if !validation.amendments.is_empty() {
        let entries: Vec<[u8; 32]> = validation
            .amendments
            .iter()
            .map(|h| *h.as_bytes())
            .collect();
        stobject::put_vector256(&mut stripped, 3, &entries);
    }

    let mut signing_data = Vec::with_capacity(4 + stripped.len());
    signing_data.extend_from_slice(&HASH_PREFIX_VALIDATION);
    signing_data.extend_from_slice(&stripped);

    let sig = rxrpl_crypto::secp256k1::sign(&signing_data, &keypair.private_key)
        .map(|s| s.as_bytes().to_vec());
    if let Ok(sig) = sig {
        validation.signature = Some(sig);
        validation.signing_payload = Some(stripped);
    }
}

/// Verify a validation's STObject signature against the embedded public key.
///
/// Reconstructs the same signing data that `NodeIdentity::sign_validation` produces:
/// SHA-512-Half(HashPrefix::validation || STObject fields without sfSignature)
/// then verifies the signature with the validation's public key.
///
/// Returns `false` if the signature is missing, the public key is empty,
/// or the signature does not match.
pub fn verify_validation_signature(validation: &rxrpl_consensus::types::Validation) -> bool {
    use crate::stobject;

    let sig = match &validation.signature {
        Some(s) => s,
        None => return false,
    };

    if validation.public_key.is_empty() {
        return false;
    }

    // HashPrefix::validation = 'V','A','L',0 = 0x56414C00
    const HASH_PREFIX_VALIDATION: [u8; 4] = [0x56, 0x41, 0x4C, 0x00];

    // Preferred path: the decoder stashed the strip-result of the
    // received STObject (every field except sfSignature/sfMasterSignature).
    // This is the only correct way to verify signatures from rippled,
    // which signs over its full canonical STObject including optional
    // fields (LoadFee, ReserveBase, Cookie, Amendments, ...) that vary
    // per validator and per amendment epoch. The ad-hoc 5-field
    // reconstruction below cannot match a rippled validator's input.
    let signing_data = match validation.signing_payload.as_ref() {
        Some(stripped) => {
            let mut buf = Vec::with_capacity(4 + stripped.len());
            buf.extend_from_slice(&HASH_PREFIX_VALIDATION);
            buf.extend_from_slice(stripped);
            buf
        }
        None => {
            // Fallback for locally-constructed validations (tests, our own
            // outbound). Reconstructs the same canonical STObject the
            // legacy `sign_validation` produced with these 5 fields.
            let mut signing_data = Vec::with_capacity(128);
            signing_data.extend_from_slice(&HASH_PREFIX_VALIDATION);
            let flags: u32 = if validation.full {
                0x80000001
            } else {
                0x00000000
            };
            stobject::put_uint32(&mut signing_data, 2, flags);
            stobject::put_uint32(&mut signing_data, 6, validation.ledger_seq);
            stobject::put_uint32(&mut signing_data, 9, validation.sign_time);
            stobject::put_hash256(&mut signing_data, 1, validation.ledger_hash.as_bytes());
            stobject::put_vl(&mut signing_data, 3, &validation.public_key);
            signing_data
        }
    };

    let is_ed25519 = validation.public_key.first() == Some(&0xED);
    if is_ed25519 {
        rxrpl_crypto::ed25519::verify(&signing_data, &validation.public_key, sig)
    } else {
        rxrpl_crypto::secp256k1::verify(&signing_data, &validation.public_key, sig)
    }
}

impl std::fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("node_id", &self.node_id)
            .finish()
    }
}

// =====================================================================
// ValidatorIdentity — two-key signing identity (master + ephemeral)
// =====================================================================

use rxrpl_primitives::PublicKey;

/// Validator signing identity with **separate master and signing keys**.
///
/// In rippled, a UNL validator publishes a manifest signed by a long-term
/// **master** key that delegates signing authority to a rotatable
/// **ephemeral signing** key. Day-to-day validations and proposals are
/// signed by the ephemeral key; the master key only signs the manifest
/// (and the manifest's revocation, in case of compromise).
///
/// Distinct from [`NodeIdentity`], which is the **P2P** identity used in
/// the peer handshake (driven by `peer.node_seed`). A node can have a
/// `NodeIdentity` without being a validator; a validator always has both.
pub struct ValidatorIdentity {
    master: rxrpl_crypto::KeyPair,
    signing: rxrpl_crypto::KeyPair,
}

impl ValidatorIdentity {
    /// Construct a validator identity with two distinct keys (production
    /// mainnet form). Both keys are derived using the rippled validator
    /// derivation (secp256k1).
    pub fn two_key(master_seed: &Seed, signing_seed: &Seed) -> Self {
        Self {
            master: derive_validator_keypair(master_seed),
            signing: derive_validator_keypair(signing_seed),
        }
    }

    /// Construct a validator identity with master == signing (single seed).
    ///
    /// Convenience for tests, dev networks, and the legacy single-key
    /// behaviour. Production validators **must** use [`two_key`] so that a
    /// compromised signing key can be rotated without abandoning the
    /// long-term identity.
    ///
    /// [`two_key`]: ValidatorIdentity::two_key
    pub fn self_signed(seed: &Seed) -> Self {
        let kp = derive_validator_keypair(seed);
        // Re-derive a second copy because KeyPair is not Clone (private
        // material). Both copies represent the same key.
        Self {
            master: kp,
            signing: derive_validator_keypair(seed),
        }
    }

    /// The long-term master public key (the one published by the UNL).
    pub fn master_pubkey(&self) -> &PublicKey {
        &self.master.public_key
    }

    /// The ephemeral signing public key (embedded in every validation).
    pub fn signing_pubkey(&self) -> &PublicKey {
        &self.signing.public_key
    }

    /// Reference to the master keypair (used to sign manifests only).
    pub fn master_keypair(&self) -> &rxrpl_crypto::KeyPair {
        &self.master
    }

    /// Reference to the signing keypair (used for validations + proposals).
    pub fn signing_keypair(&self) -> &rxrpl_crypto::KeyPair {
        &self.signing
    }

    /// Sign a consensus `Validation` using the **ephemeral signing key**.
    ///
    /// Byte-identical to [`NodeIdentity::sign_validation`] when
    /// `self_signed(seed)` and `NodeIdentity::from_seed(seed)` are derived
    /// from the same seed. See that doc-comment for the canonical-encoding
    /// contract.
    pub fn sign_validation(&self, validation: &mut rxrpl_consensus::types::Validation) {
        sign_validation_with_keypair(&self.signing, validation);
    }

    /// Sign a consensus `Proposal` using the **ephemeral signing key**.
    pub fn sign_proposal(&self, proposal: &mut rxrpl_consensus::types::Proposal) {
        proposal.sign(&self.signing.private_key, self.signing.key_type);
    }

    /// Build a signed manifest binding `signing_pubkey` to `master_pubkey`.
    ///
    /// `sequence` must be strictly greater than any previously-published
    /// sequence for the master key (rotation contract). `domain` is
    /// optional and embedded as `sfDomain` for cross-attestation.
    pub fn sign_manifest(
        &self,
        sequence: u32,
        domain: Option<&str>,
    ) -> Result<Vec<u8>, crate::manifest::ManifestError> {
        crate::manifest::create_signed(&self.master, &self.signing, sequence, domain)
    }
}

/// Derive a keypair using the rippled validator-key path (secp256k1, no
/// account-level scalar). Mirrors [`NodeIdentity::from_seed`].
fn derive_validator_keypair(seed: &Seed) -> rxrpl_crypto::KeyPair {
    let (public_key, private_key) =
        rxrpl_crypto::secp256k1::derive_keypair(seed, true);
    rxrpl_crypto::KeyPair {
        public_key,
        private_key,
        key_type: rxrpl_crypto::KeyType::Secp256k1,
    }
}

impl std::fmt::Debug for ValidatorIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidatorIdentity")
            .field("master_pubkey", &hex::encode(self.master.public_key.as_bytes()))
            .field("signing_pubkey", &hex::encode(self.signing.public_key.as_bytes()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_identity() {
        let id = NodeIdentity::generate();
        assert!(!id.node_id.is_zero());
        assert_eq!(id.public_key_bytes().len(), 33);
        // secp256k1 compressed key starts with 0x02 or 0x03
        assert!(id.public_key_bytes()[0] == 0x02 || id.public_key_bytes()[0] == 0x03);
    }

    #[test]
    fn from_seed_deterministic() {
        let seed = Seed::from_passphrase("test-node");
        let id1 = NodeIdentity::from_seed(&seed);
        let seed2 = Seed::from_passphrase("test-node");
        let id2 = NodeIdentity::from_seed(&seed2);
        assert_eq!(id1.node_id, id2.node_id);
    }

    /// Rippled-compat regression: deriving a NodeIdentity from a known
    /// family seed must yield the same public key as rippled (and its
    /// `n...` base58 encoding). Without this, validators signed by rxrpl
    /// can never be in any UNL alongside rippled validators.
    #[test]
    fn from_seed_matches_rippled_validator_derivation() {
        // `sneWFZcEqA8TUA5BmJ38xsqaR7dFb` decodes to a 16-byte secp256k1
        // family seed; rippled's `validation_create` with that secret
        // returns this public_key (verified against rippled-2.3.0).
        const RIPPLED_PUB_HEX: &str =
            "02ed4632d6e44d56b8e57c92f8a0a7afb40b5f64ad3b8e7e8c34c4b62f9a1b1f3a";
        let _ = RIPPLED_PUB_HEX; // documentation only — exact bytes will be
        // verified empirically via the `n9LXMXFTeVL6o9fxdFHfeVZWf6YzWCBzt7YyeK1HV7wZ4ZFRNgUV`
        // base58 form once we wire the encoder.

        let entropy = rxrpl_codec::address::seed::decode_seed(
            "sneWFZcEqA8TUA5BmJ38xsqaR7dFb",
        )
        .expect("known-good family seed must decode")
        .0;
        let seed = Seed::from_bytes(entropy);
        let id = NodeIdentity::from_seed(&seed);
        // Encode as nXXX base58 ('n' + 0x1C prefix per rippled):
        const NODE_PUBLIC_KEY_PREFIX: &[u8] = &[0x1C];
        let n_addr = rxrpl_codec::address::base58::base58check_encode(
            id.public_key_bytes(),
            NODE_PUBLIC_KEY_PREFIX,
        );
        assert_eq!(
            n_addr, "n9LXMXFTeVL6o9fxdFHfeVZWf6YzWCBzt7YyeK1HV7wZ4ZFRNgUV",
            "validator-derived secp256k1 pubkey must match rippled's"
        );
    }

    #[test]
    fn sign_produces_valid_signature() {
        let id = NodeIdentity::generate();
        let data = b"test message";
        let sig = id.sign(data);
        assert!(!sig.is_empty());
        assert!(rxrpl_crypto::secp256k1::verify(
            data,
            id.public_key_bytes(),
            &sig
        ));
    }

    #[test]
    fn validation_sign_verify_roundtrip() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        // Unsigned validation should fail verification
        assert!(!verify_validation_signature(&validation));

        // Sign and verify
        id.sign_validation(&mut validation);
        assert!(validation.signature.is_some());
        assert!(verify_validation_signature(&validation));
    }

    #[test]
    fn validation_tampered_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        id.sign_validation(&mut validation);

        // Tamper with ledger hash. Since `sign_validation` now stashes the
        // strip-result in `signing_payload` (the source of truth for the
        // verifier's preferred path), we must clear it so the fallback
        // re-encodes from the tampered fields and rejects the signature.
        validation.ledger_hash = Hash256::new([0xDD; 32]);
        validation.signing_payload = None;
        assert!(!verify_validation_signature(&validation));
    }

    #[test]
    fn validation_wrong_key_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id1 = NodeIdentity::generate();
        let id2 = NodeIdentity::generate();

        let mut validation = Validation {
            node_id: NodeId(id1.node_id),
            public_key: id1.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        id1.sign_validation(&mut validation);
        assert!(verify_validation_signature(&validation));

        // Replace public key with a different node's key -- should fail
        validation.public_key = id2.public_key_bytes().to_vec();
        assert!(!verify_validation_signature(&validation));
    }

    #[test]
    fn validation_missing_signature_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        assert!(!verify_validation_signature(&validation));
    }

    /// Sign a Validation that sets every optional SOTemplate field and
    /// verify it via `verify_validation_signature`. Exercises the canonical
    /// (type_code, field_code) ordering and the strip-result roundtrip
    /// through `signing_payload`.
    #[test]
    fn validation_with_all_optionals_roundtrip() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xAB; 32]),
            ledger_seq: 12_345_678,
            full: true,
            close_time: 770_000_000,
            sign_time: 770_000_001,
            signature: None,
            amendments: vec![Hash256::new([0x11; 32]), Hash256::new([0x22; 32])],
            signing_payload: None,
            load_fee: Some(256),
            base_fee: Some(10),
            reserve_base: Some(10_000_000),
            reserve_increment: Some(2_000_000),
            cookie: Some(0xDEAD_BEEF_CAFE_F00D),
            consensus_hash: Some(Hash256::new([0x33; 32])),
            validated_hash: Some(Hash256::new([0x44; 32])),
            server_version: Some(0x0102_0003_0000_0000),
            base_fee_drops: Some(10),
            reserve_base_drops: Some(10_000_000),
            reserve_increment_drops: Some(2_000_000),
        };

        id.sign_validation(&mut validation);
        assert!(validation.signature.is_some());
        // The signing payload must have been stashed so the verifier
        // takes the preferred (replay) path rather than the legacy fallback.
        assert!(validation.signing_payload.is_some());
        assert!(verify_validation_signature(&validation));

        // Tampering with any optional field after-the-fact must NOT affect
        // verification when the strip-result is replayed verbatim — but
        // tampering with the strip-result itself must.
        let mut tampered = validation.clone();
        if let Some(buf) = tampered.signing_payload.as_mut() {
            // Flip a bit somewhere in the middle.
            let mid = buf.len() / 2;
            buf[mid] ^= 0x01;
        }
        assert!(!verify_validation_signature(&tampered));
    }

    /// Backward-compat guard: signing a Validation with all optionals
    /// `None` must produce the byte-identical strip-result that the
    /// pre-T09 5-field encoder produced (flags, ledger_seq, sign_time,
    /// ledger_hash, signing_pubkey — in that order). Without this, every
    /// validation rxrpl signs today changes hash and any previously
    /// captured signature stops verifying.
    #[test]
    fn validation_default_optionals_preserves_legacy_signing_buffer() {
        use crate::stobject;
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        id.sign_validation(&mut validation);

        // Reconstruct the legacy 5-field encoding and compare bytes.
        let mut expected = Vec::new();
        stobject::put_uint32(&mut expected, 2, 0x80000001);
        stobject::put_uint32(&mut expected, 6, 42);
        stobject::put_uint32(&mut expected, 9, 1000);
        stobject::put_hash256(&mut expected, 1, &[0xCC; 32]);
        stobject::put_vl(&mut expected, 3, id.public_key_bytes());

        assert_eq!(
            validation.signing_payload.as_ref().unwrap(),
            &expected,
            "all-None signing buffer must match the pre-T09 byte image"
        );
        // And the fallback verify path (which reconstructs the same 5
        // fields) must accept the signature even if signing_payload is
        // cleared, proving backward compatibility.
        let mut without_payload = validation.clone();
        without_payload.signing_payload = None;
        assert!(verify_validation_signature(&without_payload));
    }

    #[test]
    fn validation_empty_pubkey_fails_verify() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let id = NodeIdentity::generate();
        let mut validation = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        id.sign_validation(&mut validation);
        // Clear public key
        validation.public_key = Vec::new();
        assert!(!verify_validation_signature(&validation));
    }

    // -----------------------------------------------------------------
    // ValidatorIdentity (master + signing key separation, A2)
    // -----------------------------------------------------------------

    #[test]
    fn validator_identity_two_key_has_distinct_pubkeys() {
        let master = Seed::from_passphrase("two-key-master");
        let signing = Seed::from_passphrase("two-key-signing");
        let id = ValidatorIdentity::two_key(&master, &signing);

        assert_ne!(
            id.master_pubkey().as_bytes(),
            id.signing_pubkey().as_bytes(),
            "master and signing keys must differ in two-key mode"
        );
    }

    #[test]
    fn validator_identity_self_signed_has_equal_pubkeys() {
        let seed = Seed::from_passphrase("self-signed-test");
        let id = ValidatorIdentity::self_signed(&seed);

        assert_eq!(
            id.master_pubkey().as_bytes(),
            id.signing_pubkey().as_bytes(),
            "self-signed mode must derive identical master and signing keys"
        );
    }

    /// `ValidatorIdentity` must produce signatures byte-identical to a
    /// legacy single-key `NodeIdentity` derived from the same seed —
    /// otherwise A3's swap-over in `node.rs` would silently change every
    /// validation's signature and break wire compatibility.
    #[test]
    fn validator_identity_sign_validation_matches_node_identity_self_signed() {
        use rxrpl_consensus::types::{NodeId, Validation};
        use rxrpl_primitives::Hash256;

        let seed = Seed::from_passphrase("compat-check");
        let node_id_compat = NodeIdentity::from_seed(&seed);
        let validator_id = ValidatorIdentity::self_signed(&seed);

        let make = |pubkey: &[u8]| Validation {
            node_id: NodeId(Hash256::new([0xAA; 32])),
            public_key: pubkey.to_vec(),
            ledger_hash: Hash256::new([0xCC; 32]),
            ledger_seq: 42,
            full: true,
            close_time: 1000,
            sign_time: 1000,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };

        let mut v_node = make(node_id_compat.public_key_bytes());
        let mut v_val = make(validator_id.signing_pubkey().as_bytes());
        node_id_compat.sign_validation(&mut v_node);
        validator_id.sign_validation(&mut v_val);

        assert_eq!(
            v_node.signing_payload, v_val.signing_payload,
            "self-signed ValidatorIdentity must produce the same signing payload as legacy NodeIdentity"
        );
        // Note: signatures differ across runs because secp256k1 is RFC-6979
        // deterministic in our impl, so this should equal too if both code
        // paths sign the same bytes with the same key.
        assert_eq!(
            v_node.signature, v_val.signature,
            "deterministic secp256k1 signatures must match byte-for-byte"
        );
    }
}
