//! End-to-end proof of the DYNAMIC validator-list (VL) trust path with REAL
//! cryptography.
//!
//! In production this path is gated on a configured `validator_list_site`, so
//! it is a no-op under the static/hive configurations used by every existing
//! integration test — it has therefore never been exercised end-to-end, only
//! unit-tested in disconnected pieces. This test wires the whole chain together
//! with real signed data (no mocks, no placeholder manifests):
//!
//!   publisher-signed VL  ->  `validator_list::verify_and_parse`
//!        -> per-validator manifests -> `ManifestStore` (ephemeral -> master)
//!        -> `ConsensusEngine::set_trusted_master_keys` (UNL + validations-trie)
//!        -> ephemeral-signed `Validation` counted toward quorum because the
//!           manifest resolves its signing key to a trusted VL master.
//!
//! The three claims proven live:
//!   * B1a — an EPHEMERAL-signed validation is trusted and reaches quorum
//!     because a real manifest maps its signing key to a VL master.
//!   * B1b — the UNL size, its 80%-derived quorum, and the validations-trie
//!     trusted_count are all installed from the VL masters.
//!   * B1c — those same validations, re-keyed to their master identity, drive
//!     the engine's validations-trie `get_preferred`.
//!
//! Everything is deterministic (seed-derived keys, injected sign time), so the
//! test runs unconditionally — it is NOT `#[ignore]`.

use base64::Engine;

use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{ConsensusAdapter, ConsensusEngine, ConsensusParams};
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET;
use rxrpl_overlay::manifest::{self, ManifestStore};
use rxrpl_overlay::validation_aggregator::ValidationAggregator;
use rxrpl_overlay::validator_list;
use rxrpl_overlay::vl_fetcher::new_trusted_keys;
use rxrpl_primitives::{Hash256, PublicKey};

/// Mint an ed25519 keypair from a passphrase (the VL/manifest path accepts
/// ed25519 — see the ed25519 branches in `manifest::verify_signature` and
/// `validator_list::verify_blob_signature`, and every rxrpl validation test).
fn kp(seed: &str) -> KeyPair {
    KeyPair::from_seed(&Seed::from_passphrase(seed), KeyType::Ed25519)
}

/// A validator identity: a permanent master key, a rotating ephemeral signing
/// key, and a REAL manifest binding the two (built via the production
/// `manifest::create_signed`, i.e. both signatures over the canonical
/// `HashPrefix::MANIFEST || body` bytes).
struct ValidatorId {
    master: KeyPair,
    ephemeral: KeyPair,
    manifest_bytes: Vec<u8>,
}

fn make_validator(tag: &str) -> ValidatorId {
    let master = kp(&format!("{tag}_master"));
    let ephemeral = kp(&format!("{tag}_ephemeral"));
    // Real, verifiable manifest: sequence 1, no domain. `create_signed` signs
    // the body with BOTH the ephemeral and master keys, exactly as rippled's
    // `Manifest::makeManifest`.
    let manifest_bytes = manifest::create_signed(&master, &ephemeral, 1, None)
        .expect("build a real signed validator manifest");
    ValidatorId {
        master,
        ephemeral,
        manifest_bytes,
    }
}

/// Assemble the VL blob JSON, base64 it, and sign the DECODED blob bytes with
/// the publisher's EPHEMERAL key — matching `verify_and_parse`, which decodes
/// first and verifies the signature over the decoded list JSON.
///
/// Returns `(blob_base64, signature_hex)` as the raw byte buffers the parser
/// consumes.
fn build_signed_vl_blob(
    publisher_eph: &KeyPair,
    sequence: u64,
    expiration: u64,
    validators: &[ValidatorId],
) -> (Vec<u8>, Vec<u8>) {
    let validator_entries: Vec<serde_json::Value> = validators
        .iter()
        .map(|v| {
            serde_json::json!({
                // master public key, hex (rippled uppercases it)
                "validation_public_key": hex::encode_upper(v.master.public_key.as_bytes()),
                // REAL per-validator manifest, base64 — this is the ephemeral
                // -> master binding the placeholder VLs never carried.
                "manifest": base64::engine::general_purpose::STANDARD.encode(&v.manifest_bytes),
            })
        })
        .collect();

    let blob_json = serde_json::json!({
        "sequence": sequence,
        "expiration": expiration,
        "validators": validator_entries,
    });

    let blob_raw = serde_json::to_vec(&blob_json).expect("serialize blob JSON");
    let blob_b64 = base64::engine::general_purpose::STANDARD.encode(&blob_raw);
    // Publisher signs the DECODED bytes with its EPHEMERAL key.
    let sig = rxrpl_crypto::ed25519::sign(&blob_raw, &publisher_eph.private_key)
        .expect("sign the VL blob with the publisher ephemeral key");
    (
        blob_b64.into_bytes(),
        hex::encode(sig.as_bytes()).into_bytes(),
    )
}

/// Current XRPL ripple time (u32 seconds since 2000-01-01), computed the same
/// way `ValidationAggregator::add_validation` computes `now`, so validations
/// signed "now" sit inside the `is_current` freshness window.
fn ripple_now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_secs()
        .saturating_sub(RIPPLE_EPOCH_OFFSET) as u32
}

/// Build a FULL validation for `(seq, ledger_hash)` SIGNED for real by the
/// validator's EPHEMERAL key, with `master_public_key` set to the master the
/// manifest store resolved the ephemeral key to. This is the exact shape
/// `peer_manager` produces on the wire: node id + public key are the ephemeral
/// signing identity; `trusted_key()` returns the resolved master.
fn signed_ephemeral_validation(
    ephemeral: &KeyPair,
    resolved_master: &PublicKey,
    seq: u32,
    ledger_hash: Hash256,
    now: u32,
) -> Validation {
    let mut v = Validation {
        node_id: NodeId::from_public_key(ephemeral.public_key.as_bytes()),
        public_key: ephemeral.public_key.as_bytes().to_vec(),
        ledger_hash,
        ledger_seq: seq,
        full: true,
        close_time: now,
        sign_time: now,
        signature: None,
        master_public_key: Some(resolved_master.as_bytes().to_vec()),
        ..Default::default()
    };
    // REAL ed25519 signature over the validation signing payload.
    v.sign(&ephemeral.private_key, KeyType::Ed25519);
    // Prove the signature is genuine (verifies against the ephemeral key).
    assert!(
        v.verify(ephemeral.public_key.as_bytes()),
        "ephemeral-signed validation must verify against its signing key"
    );
    v
}

/// A no-op `ConsensusAdapter`: this test drives the engine's trust plumbing
/// directly (`set_trusted_master_keys`, `record_trusted_validation`) and never
/// runs a consensus round, so none of these callbacks fire.
struct NoopAdapter;

impl ConsensusAdapter for NoopAdapter {
    fn propose(&self, _proposal: &Proposal) {}
    fn share_position(&self, _proposal: &Proposal) {}
    fn share_tx(&self, _tx_hash: &Hash256, _tx_data: &[u8]) {}
    fn acquire_tx_set(&self, _hash: &Hash256) -> Option<TxSet> {
        None
    }
    fn on_close(&self, _hash: &Hash256, _seq: u32, _close_time: u32, _tx_set: &TxSet) {}
    fn on_accept(&self, _validation: &Validation) {}
    fn on_accept_ledger(&self, _tx_set: &TxSet, _close_time: u32, _close_flags: u8) -> Hash256 {
        Hash256::ZERO
    }
}

#[tokio::test]
async fn dynamic_vl_trust_end_to_end_with_real_crypto() {
    // ---------------------------------------------------------------
    // 1. Mint the publisher (master + ephemeral) and 4 validators.
    // ---------------------------------------------------------------
    let publisher_master = kp("dynvl_publisher_master");
    let publisher_ephemeral = kp("dynvl_publisher_ephemeral");

    let validators: Vec<ValidatorId> = ["dynvl_v0", "dynvl_v1", "dynvl_v2", "dynvl_v3"]
        .iter()
        .map(|t| make_validator(t))
        .collect();
    assert_eq!(validators.len(), 4);

    // ---------------------------------------------------------------
    // 2/3. Build the publisher manifest and the signed VL blob.
    // ---------------------------------------------------------------
    let publisher_manifest =
        manifest::create_signed(&publisher_master, &publisher_ephemeral, 1, None)
            .expect("build a real signed publisher manifest");
    let (blob_b64, sig_hex) =
        build_signed_vl_blob(&publisher_ephemeral, 42, 4_000_000_000, &validators);

    // ---------------------------------------------------------------
    // 4. Run the produced VL through verify_and_parse. If our producer
    //    were wrong the signature would be rejected; acceptance proves the
    //    VL is real and correctly formatted.
    // ---------------------------------------------------------------
    let mut publisher_store = ManifestStore::new();
    let vl = validator_list::verify_and_parse(
        &publisher_manifest,
        &blob_b64,
        &sig_hex,
        &mut publisher_store,
    )
    .expect("verify_and_parse must accept our real, publisher-signed VL");

    assert_eq!(vl.sequence, 42, "parsed VL sequence");
    assert_eq!(vl.validators.len(), 4, "VL carries the 4 validator masters");
    assert_eq!(
        vl.validator_manifests.len(),
        4,
        "VL carries the 4 real validator manifests"
    );
    assert_eq!(
        vl.publisher_master_key, publisher_master.public_key,
        "publisher master key recovered from the publisher manifest"
    );
    // The parsed masters are exactly our four (as a set).
    let parsed_masters: std::collections::HashSet<Vec<u8>> = vl
        .validators
        .iter()
        .map(|pk| pk.as_bytes().to_vec())
        .collect();
    let expected_masters: std::collections::HashSet<Vec<u8>> = validators
        .iter()
        .map(|v| v.master.public_key.as_bytes().to_vec())
        .collect();
    assert_eq!(
        parsed_masters, expected_masters,
        "verify_and_parse returned exactly our four validator master keys"
    );

    // ---------------------------------------------------------------
    // 5. Feed each real validator manifest into a fresh ManifestStore and
    //    assert the ephemeral -> master mapping resolves for all 4.
    // ---------------------------------------------------------------
    let mut manifest_store = ManifestStore::new();
    for raw in &vl.validator_manifests {
        let m = manifest::parse_and_verify(raw)
            .expect("each VL-carried validator manifest must verify");
        assert!(manifest_store.apply(m), "manifest applied to the store");
    }
    for v in &validators {
        let resolved = manifest_store
            .master_key_for_ephemeral(&v.ephemeral.public_key)
            .expect("ephemeral key resolves to a master via its manifest");
        assert_eq!(
            resolved, &v.master.public_key,
            "manifest maps this validator's ephemeral key to its master key"
        );
    }

    // The trusted master set installed into the UNL / aggregator / trie.
    let masters: Vec<PublicKey> = validators
        .iter()
        .map(|v| v.master.public_key.clone())
        .collect();

    // ---------------------------------------------------------------
    // 6. B1b — install the VL masters as the engine UNL and check the
    //    derived quorum + validations-trie trusted set.
    // ---------------------------------------------------------------
    let mut engine = ConsensusEngine::new(
        NoopAdapter,
        NodeId::from_public_key(kp("dynvl_local_node").public_key.as_bytes()),
        ConsensusParams::default(),
    );
    // Before: solo mode (no UNL, empty trie).
    assert_eq!(engine.unl().effective_size(), 0);
    assert_eq!(engine.validations_trie().trusted_count(), 0);

    engine.set_trusted_master_keys(&masters);

    assert_eq!(
        engine.unl().effective_size(),
        4,
        "B1b: UNL effective size equals the 4 VL masters"
    );
    assert_eq!(
        engine.unl().quorum_threshold(),
        4,
        "B1b: quorum is ceil(4 * 0.8) == 4"
    );
    assert_eq!(
        engine.validations_trie().trusted_count(),
        4,
        "B1b: validations-trie trusted_count follows the VL"
    );
    // Each VL master (by NodeId) is trusted in the UNL.
    for pk in &masters {
        assert!(
            engine
                .unl()
                .is_trusted(&NodeId::from_public_key(pk.as_bytes())),
            "each VL master is trusted in the UNL"
        );
    }

    // ---------------------------------------------------------------
    // 7. B1a — an EPHEMERAL-signed validation is COUNTED because the
    //    manifest maps its signing key to a trusted VL master.
    //
    //    The aggregator's trusted set holds the 4 MASTER keys. Quorum is set
    //    to 3 (a 3-of-4 majority of the VL) so that the 3rd trusted validation
    //    trips `ValidatedLedger`; the UNL's own 80%-derived quorum of 4 is the
    //    separate B1b assertion above.
    // ---------------------------------------------------------------
    let trusted = new_trusted_keys();
    {
        let mut guard = trusted.write().await;
        for pk in &masters {
            guard.insert(pk.clone());
        }
    }
    let mut agg = ValidationAggregator::new(3).with_trusted_keys(trusted);

    let now = ripple_now();
    let seq: u32 = 9_000_001;
    let ledger_hash = Hash256::new([0x5A; 32]);

    // Direct trust checks: a VL master resolves as trusted; a non-VL key does
    // not. `is_trusted` is keyed by MASTER, matching `trusted_key()`.
    let outsider = make_validator("dynvl_outsider");
    assert!(
        agg.is_trusted(masters[0].as_bytes()),
        "a VL master key is trusted"
    );
    assert!(
        !agg.is_trusted(outsider.master.public_key.as_bytes()),
        "control: a master key not in the VL is NOT trusted"
    );
    assert!(
        !agg.is_trusted(outsider.ephemeral.public_key.as_bytes()),
        "control: a non-VL ephemeral key is NOT trusted"
    );

    // Build the three ephemeral-signed validations up front (so B1c can reuse
    // them), each resolved to its master via the manifest store.
    let mut trie_feed: Vec<Validation> = Vec::new();
    let mut quorum_result = None;
    for (i, v) in validators.iter().take(3).enumerate() {
        let resolved = manifest_store
            .master_key_for_ephemeral(&v.ephemeral.public_key)
            .expect("resolved master")
            .clone();
        let validation =
            signed_ephemeral_validation(&v.ephemeral, &resolved, seq, ledger_hash, now);

        // The validation's trusted_key is the MASTER, which is in the set,
        // even though it was SIGNED by the ephemeral key.
        assert!(
            agg.is_trusted(validation.trusted_key()),
            "B1a: ephemeral-signed validation #{i} is trusted via manifest->master"
        );

        trie_feed.push(validation.clone());
        let res = agg.add_validation(validation);
        if i < 2 {
            assert!(
                res.is_none(),
                "B1a: validation #{i} does not yet reach quorum"
            );
        } else {
            quorum_result = res;
        }
    }

    // Each of the 3 ephemeral-signed validations was COUNTED, and the 3rd
    // tripped quorum.
    assert_eq!(
        agg.validation_count(seq, &ledger_hash),
        3,
        "B1a: all three ephemeral-signed validations were counted"
    );
    let validated = quorum_result.expect("B1a: 3rd trusted validation reaches quorum");
    assert_eq!(validated.seq, seq);
    assert_eq!(validated.hash, ledger_hash);
    assert_eq!(validated.validation_count, 3);
    assert!(agg.is_validated(seq), "B1a: sequence marked validated");

    // Control: a validation whose master is NOT in the VL is dropped (a fresh
    // sequence avoids the already-validated short-circuit). It is signed for
    // real, so the ONLY reason it is rejected is the trust gate.
    let control_seq = seq + 1;
    let control = signed_ephemeral_validation(
        &outsider.ephemeral,
        &outsider.master.public_key,
        control_seq,
        ledger_hash,
        now,
    );
    assert!(
        agg.add_validation(control).is_none(),
        "control: non-VL validation is not counted toward quorum"
    );
    assert_eq!(
        agg.validation_count(control_seq, &ledger_hash),
        0,
        "control: nothing recorded for the non-VL validator"
    );

    // ---------------------------------------------------------------
    // 8. B1c — the same three validations, re-keyed to their master identity
    //    (`to_trie_identity`), drive the engine validations-trie so that
    //    `get_preferred` returns the agreed hash.
    // ---------------------------------------------------------------
    for (i, v) in trie_feed.iter().enumerate() {
        let trie_id = v.to_trie_identity();
        // to_trie_identity re-keys node id + public key to the MASTER, which
        // is exactly the trusted set installed by set_trusted_master_keys.
        assert_eq!(
            trie_id.node_id,
            NodeId::from_public_key(validators[i].master.public_key.as_bytes()),
            "B1c: trie identity is keyed by the validator master"
        );
        assert!(
            engine.record_trusted_validation(trie_id),
            "B1c: trusted validation #{i} recorded into the validations-trie"
        );
    }
    assert_eq!(
        engine.validations_trie().count_for(&ledger_hash),
        3,
        "B1c: three trusted validators support the agreed tip"
    );
    assert_eq!(
        engine.validations_trie().get_preferred(seq),
        Some(ledger_hash),
        "B1c: validations-trie prefers the ledger the VL validators agreed on"
    );
}
