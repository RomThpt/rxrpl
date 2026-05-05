/// Integration tests for Validator List v2 + Cascade Trust (Initiative D).
///
/// Covers the B5 test matrix from PLAN.md:
///   - v1 backward-compat
///   - v2 single blob in window
///   - v2 all blobs expired (no active set)
///   - v2 + cascade one level
///   - v2 + cascade depth exceeded
///   - cascade revocation
///   - mixed v1+v2 publishers
use base64::Engine;

use rxrpl_overlay::manifest::{self as manifest_mod, MANIFEST_REVOKED_SEQ, ManifestStore};
use rxrpl_overlay::validator_list::{
    BlobV2Wire, CASCADE_DEPTH_DEFAULT, DelegateResolver, ValidatorListError, resolve_cascade,
    verify_and_parse, verify_and_parse_v2,
};
use rxrpl_primitives::PublicKey;

/// Build a publisher manifest from a master + ephemeral seed pair.
fn build_publisher_manifest(
    master_seed: &str,
    eph_seed: &str,
) -> (rxrpl_crypto::KeyPair, rxrpl_crypto::KeyPair, Vec<u8>) {
    let pub_kp = rxrpl_crypto::KeyPair::from_seed(
        &rxrpl_crypto::Seed::from_passphrase(master_seed),
        rxrpl_crypto::KeyType::Ed25519,
    );
    let eph_kp = rxrpl_crypto::KeyPair::from_seed(
        &rxrpl_crypto::Seed::from_passphrase(eph_seed),
        rxrpl_crypto::KeyType::Ed25519,
    );
    let signing_data = manifest_mod::build_signing_data(
        1,
        pub_kp.public_key.as_bytes(),
        eph_kp.public_key.as_bytes(),
        None,
    );
    let eph_sig = rxrpl_crypto::ed25519::sign(&signing_data, &eph_kp.private_key).unwrap();
    let master_sig = rxrpl_crypto::ed25519::sign(&signing_data, &pub_kp.private_key).unwrap();
    let bytes = manifest_mod::build_manifest_bytes(
        1,
        pub_kp.public_key.as_bytes(),
        eph_kp.public_key.as_bytes(),
        eph_sig.as_bytes(),
        master_sig.as_bytes(),
        None,
    );
    (pub_kp, eph_kp, bytes)
}

/// Sign and base64-encode a v1 blob (single blob, no version field).
fn sign_v1_blob(
    eph_kp: &rxrpl_crypto::KeyPair,
    sequence: u64,
    expiration: u64,
    validator_seeds: &[&str],
) -> (Vec<u8>, Vec<u8>) {
    let validator_entries: Vec<serde_json::Value> = validator_seeds
        .iter()
        .map(|v| {
            let kp = rxrpl_crypto::KeyPair::from_seed(
                &rxrpl_crypto::Seed::from_passphrase(v),
                rxrpl_crypto::KeyType::Ed25519,
            );
            serde_json::json!({
                "validation_public_key": hex::encode_upper(kp.public_key.as_bytes()),
                "manifest": base64::engine::general_purpose::STANDARD.encode(b"placeholder"),
            })
        })
        .collect();
    let blob_json = serde_json::json!({
        "sequence": sequence,
        "expiration": expiration,
        "validators": validator_entries,
    });
    let blob_b64 =
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&blob_json).unwrap());
    let sig = rxrpl_crypto::ed25519::sign(blob_b64.as_bytes(), &eph_kp.private_key).unwrap();
    (
        blob_b64.into_bytes(),
        hex::encode(sig.as_bytes()).into_bytes(),
    )
}

/// Build one BlobV2Wire signed with `eph_kp`.
fn sign_v2_blob(
    eph_kp: &rxrpl_crypto::KeyPair,
    effective_start: u64,
    effective_expiration: u64,
    sequence: u64,
    validator_seeds: &[&str],
    delegates: &[PublicKey],
) -> BlobV2Wire {
    let validator_entries: Vec<serde_json::Value> = validator_seeds
        .iter()
        .map(|v| {
            let kp = rxrpl_crypto::KeyPair::from_seed(
                &rxrpl_crypto::Seed::from_passphrase(v),
                rxrpl_crypto::KeyType::Ed25519,
            );
            serde_json::json!({
                "validation_public_key": hex::encode_upper(kp.public_key.as_bytes()),
                "manifest": base64::engine::general_purpose::STANDARD.encode(b"placeholder"),
            })
        })
        .collect();
    let delegate_entries: Vec<String> = delegates
        .iter()
        .map(|pk| hex::encode_upper(pk.as_bytes()))
        .collect();
    let blob_json = serde_json::json!({
        "sequence": sequence,
        "expiration": effective_expiration,
        "validators": validator_entries,
        "delegates": delegate_entries,
    });
    let blob_b64 =
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&blob_json).unwrap());
    let sig = rxrpl_crypto::ed25519::sign(blob_b64.as_bytes(), &eph_kp.private_key).unwrap();
    BlobV2Wire {
        effective_start,
        effective_expiration,
        blob_base64: blob_b64.into_bytes(),
        signature_hex: hex::encode(sig.as_bytes()).into_bytes(),
    }
}

/// Mock cascade resolver backed by an in-memory map.
#[derive(Default)]
struct MapResolver {
    entries: std::collections::HashMap<Vec<u8>, (Vec<u8>, Vec<BlobV2Wire>)>,
}

impl MapResolver {
    fn insert(&mut self, pk: &PublicKey, manifest: Vec<u8>, wire: Vec<BlobV2Wire>) {
        self.entries
            .insert(pk.as_bytes().to_vec(), (manifest, wire));
    }
}

impl DelegateResolver for MapResolver {
    fn resolve(
        &mut self,
        delegate_pk: &PublicKey,
    ) -> Result<Option<(Vec<u8>, Vec<BlobV2Wire>)>, ValidatorListError> {
        Ok(self.entries.get(&delegate_pk.as_bytes().to_vec()).cloned())
    }
}

#[test]
fn b5_v1_backward_compat() {
    let (_pub_kp, eph_kp, manifest_bytes) = build_publisher_manifest("b5_v1_master", "b5_v1_eph");
    let (blob, sig) = sign_v1_blob(&eph_kp, 1, 999_999_999, &["b5_v1_val_a", "b5_v1_val_b"]);

    let mut store = ManifestStore::new();
    let vl = verify_and_parse(&manifest_bytes, &blob, &sig, &mut store)
        .expect("v1 path must keep working");
    assert_eq!(vl.sequence, 1);
    assert_eq!(vl.validators.len(), 2);
}

#[test]
fn b5_v2_single_blob_in_window() {
    let (_pub_kp, eph_kp, manifest_bytes) =
        build_publisher_manifest("b5_v2_single_master", "b5_v2_single_eph");
    let wire = vec![sign_v2_blob(
        &eph_kp,
        0,
        1000,
        1,
        &["b5_v2_single_val"],
        &[],
    )];

    let mut store = ManifestStore::new();
    let bundle = verify_and_parse_v2(&manifest_bytes, &wire, &mut store, 100)
        .expect("v2 single blob must parse");
    assert_eq!(bundle.active.len(), 1);
    assert_eq!(bundle.inactive.len(), 0);
    assert_eq!(bundle.active[0].base.validators.len(), 1);
}

#[test]
fn b5_v2_all_blobs_expired() {
    let (_pub_kp, eph_kp, manifest_bytes) =
        build_publisher_manifest("b5_v2_expired_master", "b5_v2_expired_eph");
    let wire = vec![
        sign_v2_blob(&eph_kp, 0, 50, 1, &["b5_v2_expired_a"], &[]),
        sign_v2_blob(&eph_kp, 200, 300, 2, &["b5_v2_expired_b"], &[]),
    ];

    let mut store = ManifestStore::new();
    // now=100: blob1 ended at 50; blob2 not yet started.
    let bundle = verify_and_parse_v2(&manifest_bytes, &wire, &mut store, 100)
        .expect("parse must still succeed (signature-wise)");
    assert!(bundle.active.is_empty(), "no blob is currently effective");
    assert_eq!(bundle.inactive.len(), 2);
}

#[test]
fn b5_v2_cascade_one_level() {
    let (_pp, _pe, primary_manifest) =
        build_publisher_manifest("b5_casc_primary", "b5_casc_primary_eph");
    let (dp, de, delegate_manifest) =
        build_publisher_manifest("b5_casc_delegate", "b5_casc_delegate_eph");
    let delegate_pk = dp.public_key.clone();

    let primary_wire = vec![sign_v2_blob(
        &_pe,
        0,
        1000,
        1,
        &["b5_casc_val_primary"],
        &[delegate_pk.clone()],
    )];
    let delegate_wire = vec![sign_v2_blob(
        &de,
        0,
        1000,
        1,
        &["b5_casc_val_delegate"],
        &[],
    )];

    let mut store = ManifestStore::new();
    let bundle = verify_and_parse_v2(&primary_manifest, &primary_wire, &mut store, 100).unwrap();
    let mut resolver = MapResolver::default();
    resolver.insert(&delegate_pk, delegate_manifest, delegate_wire);

    let merged = resolve_cascade(
        bundle.active,
        &mut resolver,
        &mut store,
        100,
        CASCADE_DEPTH_DEFAULT,
    )
    .expect("cascade resolves");
    assert_eq!(merged.len(), 2);
    let total: usize = merged.iter().map(|b| b.base.validators.len()).sum();
    assert_eq!(total, 2);
}

#[test]
fn b5_v2_cascade_depth_exceeded() {
    let (_pp, primary_eph, primary_manifest) =
        build_publisher_manifest("b5_depth_primary", "b5_depth_primary_eph");
    let (d1p, d1e, d1_manifest) = build_publisher_manifest("b5_depth_d1", "b5_depth_d1_eph");
    let (d2p, d2e, d2_manifest) = build_publisher_manifest("b5_depth_d2", "b5_depth_d2_eph");
    let (d3p, d3e, d3_manifest) = build_publisher_manifest("b5_depth_d3", "b5_depth_d3_eph");
    let (_d4p, d4e, d4_manifest) = build_publisher_manifest("b5_depth_d4", "b5_depth_d4_eph");

    let primary_wire = vec![sign_v2_blob(
        &primary_eph,
        0,
        1000,
        1,
        &[],
        &[d1p.public_key.clone()],
    )];
    let d1_wire = vec![sign_v2_blob(
        &d1e,
        0,
        1000,
        1,
        &[],
        &[d2p.public_key.clone()],
    )];
    let d2_wire = vec![sign_v2_blob(
        &d2e,
        0,
        1000,
        1,
        &[],
        &[d3p.public_key.clone()],
    )];
    let d3_wire = vec![sign_v2_blob(
        &d3e,
        0,
        1000,
        1,
        &[],
        &[_d4p.public_key.clone()],
    )];
    let d4_wire = vec![sign_v2_blob(&d4e, 0, 1000, 1, &[], &[])];

    let mut store = ManifestStore::new();
    let bundle = verify_and_parse_v2(&primary_manifest, &primary_wire, &mut store, 100).unwrap();
    let mut resolver = MapResolver::default();
    resolver.insert(&d1p.public_key, d1_manifest, d1_wire);
    resolver.insert(&d2p.public_key, d2_manifest, d2_wire);
    resolver.insert(&d3p.public_key, d3_manifest, d3_wire);
    resolver.insert(&_d4p.public_key, d4_manifest, d4_wire);

    // depth_limit=2 should reject a 4-deep chain.
    let res = resolve_cascade(bundle.active, &mut resolver, &mut store, 100, 2);
    assert!(matches!(
        res,
        Err(ValidatorListError::CascadeDepthExceeded(2))
    ));
}

#[test]
fn b5_cascade_delegate_revoked() {
    let (_pp, primary_eph, primary_manifest) =
        build_publisher_manifest("b5_rev_primary", "b5_rev_primary_eph");
    let (dp, _de, _delegate_manifest) =
        build_publisher_manifest("b5_rev_delegate", "b5_rev_delegate_eph");
    let delegate_pk = dp.public_key.clone();

    let primary_wire = vec![sign_v2_blob(
        &primary_eph,
        0,
        1000,
        1,
        &[],
        &[delegate_pk.clone()],
    )];

    let mut store = ManifestStore::new();
    let bundle = verify_and_parse_v2(&primary_manifest, &primary_wire, &mut store, 100).unwrap();

    // Apply a revocation manifest for the delegate.
    let revoke = manifest_mod::Manifest {
        sequence: MANIFEST_REVOKED_SEQ,
        master_public_key: delegate_pk.clone(),
        ephemeral_public_key: None,
        domain: None,
        raw: vec![],
    };
    store.apply(revoke);
    assert!(store.is_revoked(&delegate_pk));

    let mut resolver = MapResolver::default();
    let res = resolve_cascade(
        bundle.active,
        &mut resolver,
        &mut store,
        100,
        CASCADE_DEPTH_DEFAULT,
    );
    assert!(matches!(res, Err(ValidatorListError::DelegateRevoked)));
}

#[test]
fn b5_mixed_v1_v2_publishers() {
    // Publisher A speaks v1; publisher B speaks v2. Both must work side
    // by side without interference.
    let (_pa, ea, manifest_a) = build_publisher_manifest("b5_mixed_a", "b5_mixed_a_eph");
    let (blob_a, sig_a) = sign_v1_blob(&ea, 7, 999_999_999, &["b5_mixed_val_a"]);

    let (_pb, eb, manifest_b) = build_publisher_manifest("b5_mixed_b", "b5_mixed_b_eph");
    let wire_b = vec![sign_v2_blob(&eb, 0, 1000, 3, &["b5_mixed_val_b"], &[])];

    let mut store = ManifestStore::new();
    let vl_a = verify_and_parse(&manifest_a, &blob_a, &sig_a, &mut store).expect("v1 ok");
    assert_eq!(vl_a.sequence, 7);

    let bundle_b = verify_and_parse_v2(&manifest_b, &wire_b, &mut store, 100).expect("v2 ok");
    assert_eq!(bundle_b.active.len(), 1);
    assert_eq!(bundle_b.active[0].base.sequence, 3);
}

#[test]
fn b5_cascade_resolver_unknown_delegate() {
    // Primary references a delegate the resolver has never heard of.
    let (_pp, primary_eph, primary_manifest) =
        build_publisher_manifest("b5_unknown_primary", "b5_unknown_primary_eph");
    let (dp, _de, _dm) = build_publisher_manifest("b5_unknown_d", "b5_unknown_d_eph");
    let primary_wire = vec![sign_v2_blob(
        &primary_eph,
        0,
        1000,
        1,
        &[],
        &[dp.public_key.clone()],
    )];
    let mut store = ManifestStore::new();
    let bundle = verify_and_parse_v2(&primary_manifest, &primary_wire, &mut store, 100).unwrap();
    let mut resolver = MapResolver::default();
    let res = resolve_cascade(
        bundle.active,
        &mut resolver,
        &mut store,
        100,
        CASCADE_DEPTH_DEFAULT,
    );
    assert!(matches!(
        res,
        Err(ValidatorListError::DelegateFetchFailed(_))
    ));
}
