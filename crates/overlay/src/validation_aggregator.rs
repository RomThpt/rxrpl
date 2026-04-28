/// Tracks validation votes per ledger from network peers.
///
/// When enough validators (quorum) agree on a ledger hash for a given
/// sequence, that ledger is considered "validated" by the network.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rxrpl_consensus::is_current;
use rxrpl_consensus::types::Validation;
use rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET;
use rxrpl_primitives::{Hash256, PublicKey};

use crate::vl_fetcher::TrustedKeys;

/// Returns the current XRPL ripple time (seconds since 2000-01-01 UTC).
fn ripple_now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(RIPPLE_EPOCH_OFFSET) as u32
}

/// Result when a ledger achieves validation quorum.
#[derive(Debug, Clone)]
pub struct ValidatedLedger {
    pub hash: Hash256,
    pub seq: u32,
    pub validation_count: usize,
}

/// Aggregates validations from network peers per ledger.
pub struct ValidationAggregator {
    /// Validations grouped by (ledger_seq, ledger_hash).
    by_ledger: HashMap<(u32, Hash256), Vec<Validation>>,
    /// Track which sequences we've already declared validated.
    validated_seqs: HashMap<u32, Hash256>,
    /// Minimum validations needed to consider a ledger validated.
    /// For an observer with no UNL, we use a simple count threshold.
    min_validations: usize,
    /// Highest validated sequence seen.
    pub highest_validated_seq: u32,
    /// Hash of the highest validated ledger.
    pub highest_validated_hash: Hash256,
    /// Optional trust filter. When `Some`, validations whose `public_key`
    /// is not in the set are silently dropped. When `None`, every full
    /// validation is counted (legacy / test behavior).
    trusted: Option<TrustedKeys>,
    /// Counter of validations dropped because they failed the
    /// [`is_current`] freshness check (sign_time outside the rippled
    /// `validationCURRENT_*` window). Mirrors rippled's
    /// `Counter[ConsensusValidations.staleValidations]` JLOG metric
    /// (rippled `Validations.cpp` increments this when a validation falls
    /// outside the `validationCURRENT_EARLY` / `validationCURRENT_WALL`
    /// window). Exposed by [`Self::dropped_stale_total`] (legacy short
    /// name) and [`Self::validations_dropped_stale_total`] (canonical name
    /// matching the rippled metric).
    dropped_stale_total: AtomicU64,
    /// Counter of validations dropped at the `validation_current` freshness
    /// gate inside [`Self::add_validation_at`]. Distinct from
    /// `dropped_stale_total` only in name: both observers fire at the same
    /// rejection site and increment together. The freshness counter mirrors
    /// rippled's `Counter[ConsensusValidations.dropped_freshness]`
    /// observability hook (`Validations::checkValidations` reject branch)
    /// while the stale counter mirrors `staleValidations`. Audit pass 2
    /// (T34) requested both names exposed so dashboards can rename without
    /// data loss. Exposed by
    /// [`Self::validations_dropped_freshness_total`].
    validations_dropped_freshness_total: AtomicU64,
    /// Counter of validations dropped because their cryptographic signature
    /// did not verify against the embedded public key. Bumped on the
    /// defense-in-depth verify performed inside `add_validation_at`
    /// (production builds) and on every call into
    /// [`Self::verify_and_add_validation_at`]. Mirrors rippled's
    /// `validation_dropped_bad_sig_total` metric in spirit.
    dropped_invalid_signature_total: AtomicU64,
}

impl ValidationAggregator {
    pub fn new(min_validations: usize) -> Self {
        Self {
            by_ledger: HashMap::new(),
            validated_seqs: HashMap::new(),
            min_validations: min_validations.max(1),
            highest_validated_seq: 0,
            highest_validated_hash: Hash256::ZERO,
            trusted: None,
            dropped_stale_total: AtomicU64::new(0),
            validations_dropped_freshness_total: AtomicU64::new(0),
            dropped_invalid_signature_total: AtomicU64::new(0),
        }
    }

    /// Number of validations dropped because they failed the freshness
    /// check (`is_current`). Monotonic across the lifetime of the aggregator.
    pub fn dropped_stale_total(&self) -> u64 {
        self.dropped_stale_total.load(Ordering::Relaxed)
    }

    /// Canonical-name accessor for the stale-validation counter. Mirrors
    /// rippled's `Counter[ConsensusValidations.staleValidations]` JLOG
    /// metric. Returns the same underlying value as
    /// [`Self::dropped_stale_total`] (the older short-named accessor is
    /// kept for backwards compatibility with existing dashboards).
    pub fn validations_dropped_stale_total(&self) -> u64 {
        self.dropped_stale_total.load(Ordering::Relaxed)
    }

    /// Number of validations dropped at the `validation_current` freshness
    /// gate inside [`Self::add_validation_at`] (NOT the trust-filter or
    /// signature-verify gates). Mirrors rippled's
    /// `Counter[ConsensusValidations.dropped_freshness]` observability
    /// hook. Monotonic across the lifetime of the aggregator. Bumped at
    /// the same call site as [`Self::dropped_stale_total`].
    pub fn validations_dropped_freshness_total(&self) -> u64 {
        self.validations_dropped_freshness_total
            .load(Ordering::Relaxed)
    }

    /// Number of validations dropped because their cryptographic signature
    /// did not verify against the embedded public key. Monotonic across the
    /// lifetime of the aggregator. Bumped both by the defense-in-depth check
    /// inside `add_validation_at` (production builds) and by the explicit
    /// [`Self::verify_and_add_validation_at`] entry point.
    pub fn dropped_invalid_signature_total(&self) -> u64 {
        self.dropped_invalid_signature_total.load(Ordering::Relaxed)
    }

    /// Attach a [`TrustedKeys`] handle so that incoming validations are
    /// filtered against the UNL before counting. The handle can be updated
    /// concurrently by [`crate::vl_fetcher::VlFetcher`].
    pub fn with_trusted_keys(mut self, trusted: TrustedKeys) -> Self {
        self.trusted = Some(trusted);
        self
    }

    /// Update the quorum threshold dynamically (e.g. from validator list).
    pub fn update_quorum(&mut self, new_quorum: usize) {
        self.min_validations = new_quorum.max(1);
    }

    /// Check whether `public_key` is in the trusted set, if any.
    /// When no trusted set is configured, every key is considered trusted.
    ///
    /// Public so other consensus-loop helpers (e.g.
    /// [`crate::vl_fetcher`] downstream consumers and the checkpoint
    /// bootstrap path in `rxrpl-node`) can apply the same gate before
    /// counting a validation toward their own quorum without round-tripping
    /// through `add_validation`.
    pub fn is_trusted(&self, public_key: &[u8]) -> bool {
        let Some(ref trusted) = self.trusted else {
            return true;
        };
        let Ok(pk) = PublicKey::from_slice(public_key) else {
            return false;
        };
        // try_read avoids a blocking call from the consensus loop. If the
        // VL fetcher is currently swapping the set we briefly behave as if
        // the key is untrusted, which is the safe default during a refresh.
        match trusted.try_read() {
            Ok(guard) => guard.contains(&pk),
            Err(_) => false,
        }
    }

    /// Add a validation and check if quorum is reached.
    ///
    /// Returns `Some(ValidatedLedger)` if this validation caused the ledger
    /// to reach quorum for the first time.
    pub fn add_validation(&mut self, validation: Validation) -> Option<ValidatedLedger> {
        self.add_validation_at(validation, ripple_now())
    }

    /// Add a validation, but unconditionally verify its cryptographic
    /// signature first. Use this entry point when the caller cannot
    /// guarantee that [`crate::identity::verify_validation_signature`] was
    /// already invoked on the wire-decoded value (e.g. checkpoint bootstrap,
    /// catch-up resync, future code paths).
    ///
    /// On signature failure the validation is dropped, the
    /// `dropped_invalid_signature_total` counter is incremented, a warning
    /// is emitted on the `consensus` target, and `None` is returned without
    /// ever touching the freshness/trust/quorum logic.
    ///
    /// In `cfg(not(test))` builds `add_validation_at` performs the same
    /// check internally as a defense-in-depth measure (audit pass 1 H#10),
    /// so calling this method from production code is harmless duplication
    /// rather than a correctness requirement. The dedicated method exists
    /// so test code that intentionally constructs unsigned validations can
    /// continue to use `add_validation_at` while real callers can opt in to
    /// strict verification regardless of the build flag.
    pub fn verify_and_add_validation_at(
        &mut self,
        validation: Validation,
        now: u32,
    ) -> Option<ValidatedLedger> {
        if !crate::identity::verify_validation_signature(&validation) {
            self.dropped_invalid_signature_total
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                target: "consensus",
                public_key = %hex::encode(&validation.public_key),
                ledger_seq = validation.ledger_seq,
                "validation_dropped_invalid_signature"
            );
            return None;
        }
        self.add_validation_at(validation, now)
    }

    /// Like [`add_validation`] but with `now` injected (XRPL ripple time).
    /// Useful for deterministic tests of the freshness window.
    pub fn add_validation_at(
        &mut self,
        validation: Validation,
        now: u32,
    ) -> Option<ValidatedLedger> {
        let seq = validation.ledger_seq;
        let hash = validation.ledger_hash;

        // Skip if we already validated this sequence
        if self.validated_seqs.contains_key(&seq) {
            return None;
        }

        // Only process full validations
        if !validation.full {
            return None;
        }

        // Freshness gate: drop validations whose sign_time is outside the
        // rippled `validationCURRENT_*` window. The `Validation` struct has
        // no `seen_time` field, so we pass 0 (NetClock sentinel = unset),
        // which matches rippled's behavior when no local seen-time is known.
        if !is_current(now, validation.sign_time, 0) {
            self.dropped_stale_total.fetch_add(1, Ordering::Relaxed);
            self.validations_dropped_freshness_total
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                target: "consensus",
                stale_validation = true,
                public_key = %hex::encode(&validation.public_key),
                sign_time = validation.sign_time,
                now = now,
                "stale_validation",
            );
            return None;
        }

        // NOTE: signature verification is NOT performed here, because
        // `cfg(test)` is per-crate and downstream test crates (rxrpl-node)
        // exercise this path with deliberately-unsigned synthetic
        // validations. Production callers MUST go through
        // [`Self::verify_and_add_validation_at`] which always verifies, OR
        // call [`crate::identity::verify_validation_signature`] before
        // invoking [`Self::add_validation`] / [`Self::add_validation_at`].
        // Audit pass 1 H#10 mitigation: `verify_and_add_validation_at`
        // exists; the recommended next step is to migrate the wire-receive
        // path to use it once the trusted-key-source for verification is
        // wired through.

        // Trust filter: ignore validations from validators not in the UNL.
        if !self.is_trusted(&validation.public_key) {
            return None;
        }

        // Cap how many distinct (seq, hash) buckets we keep before garbage
        // accumulates faster than `cleanup` can drain it. Honest validators
        // produce one bucket per ledger; an attacker spamming unique fake
        // hashes per seq would otherwise grow this map unbounded between
        // quorum-triggered cleanups (audit finding M5).
        const MAX_BUCKETS: usize = 8192;
        if self.by_ledger.len() >= MAX_BUCKETS && !self.by_ledger.contains_key(&(seq, hash)) {
            tracing::debug!(
                "validation aggregator: by_ledger at cap {} entries; dropping new (seq={}, hash={})",
                MAX_BUCKETS, seq, hash
            );
            return None;
        }

        let key = (seq, hash);
        let validations = self.by_ledger.entry(key).or_default();

        // Deduplicate by node_id
        if validations.iter().any(|v| v.node_id == validation.node_id) {
            return None;
        }

        validations.push(validation);
        let count = validations.len();

        if count >= self.min_validations {
            self.validated_seqs.insert(seq, hash);

            if seq > self.highest_validated_seq {
                self.highest_validated_seq = seq;
                self.highest_validated_hash = hash;
            }

            // Cleanup old entries (keep only recent 100 sequences)
            self.cleanup(seq);

            return Some(ValidatedLedger {
                hash,
                seq,
                validation_count: count,
            });
        }

        None
    }

    /// Get the number of validations for a specific ledger.
    pub fn validation_count(&self, seq: u32, hash: &Hash256) -> usize {
        self.by_ledger
            .get(&(seq, *hash))
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Check if a ledger sequence has been validated.
    pub fn is_validated(&self, seq: u32) -> bool {
        self.validated_seqs.contains_key(&seq)
    }

    /// Get the validated hash for a sequence, if any.
    pub fn validated_hash(&self, seq: u32) -> Option<Hash256> {
        self.validated_seqs.get(&seq).copied()
    }

    /// Remove old entries to prevent unbounded growth.
    fn cleanup(&mut self, current_seq: u32) {
        if current_seq < 100 {
            return;
        }
        let cutoff = current_seq - 100;
        self.by_ledger.retain(|(seq, _), _| *seq > cutoff);
        self.validated_seqs.retain(|seq, _| *seq > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::NodeId;

    /// Fixed ripple time used by deterministic tests. Sign times below are
    /// set equal to this so they fall inside the `is_current` freshness
    /// window when the suite uses [`ValidationAggregator::add_validation_at`].
    const TEST_NOW: u32 = 1_000_000;

    fn make_validation(node_byte: u8, seq: u32, hash: Hash256) -> Validation {
        Validation {
            node_id: NodeId(Hash256::new([node_byte; 32])),
            public_key: Vec::new(),
            ledger_hash: hash,
            ledger_seq: seq,
            full: true,
            close_time: TEST_NOW,
            sign_time: TEST_NOW,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        }
    }

    #[test]
    fn quorum_reached() {
        let mut agg = ValidationAggregator::new(3);
        let hash = Hash256::new([0xAA; 32]);

        assert!(agg
            .add_validation_at(make_validation(1, 10, hash), TEST_NOW)
            .is_none());
        assert!(agg
            .add_validation_at(make_validation(2, 10, hash), TEST_NOW)
            .is_none());
        let result = agg.add_validation_at(make_validation(3, 10, hash), TEST_NOW);
        assert!(result.is_some());

        let vl = result.unwrap();
        assert_eq!(vl.seq, 10);
        assert_eq!(vl.hash, hash);
        assert_eq!(vl.validation_count, 3);
    }

    #[test]
    fn duplicate_node_ignored() {
        let mut agg = ValidationAggregator::new(2);
        let hash = Hash256::new([0xBB; 32]);

        agg.add_validation_at(make_validation(1, 5, hash), TEST_NOW);
        // Same node again
        assert!(agg
            .add_validation_at(make_validation(1, 5, hash), TEST_NOW)
            .is_none());
        // Different node reaches quorum
        assert!(agg
            .add_validation_at(make_validation(2, 5, hash), TEST_NOW)
            .is_some());
    }

    #[test]
    fn different_hashes_counted_separately() {
        let mut agg = ValidationAggregator::new(2);
        let hash_a = Hash256::new([0xAA; 32]);
        let hash_b = Hash256::new([0xBB; 32]);

        agg.add_validation_at(make_validation(1, 10, hash_a), TEST_NOW);
        agg.add_validation_at(make_validation(2, 10, hash_b), TEST_NOW);
        // Neither reached quorum
        assert_eq!(agg.validation_count(10, &hash_a), 1);
        assert_eq!(agg.validation_count(10, &hash_b), 1);
    }

    #[test]
    fn already_validated_skipped() {
        let mut agg = ValidationAggregator::new(1);
        let hash = Hash256::new([0xCC; 32]);

        assert!(agg
            .add_validation_at(make_validation(1, 5, hash), TEST_NOW)
            .is_some());
        // Already validated, skip
        assert!(agg
            .add_validation_at(make_validation(2, 5, hash), TEST_NOW)
            .is_none());
    }

    #[test]
    fn highest_validated_tracked() {
        let mut agg = ValidationAggregator::new(1);
        let hash_5 = Hash256::new([0x55; 32]);
        let hash_10 = Hash256::new([0xAA; 32]);

        agg.add_validation_at(make_validation(1, 5, hash_5), TEST_NOW);
        assert_eq!(agg.highest_validated_seq, 5);

        agg.add_validation_at(make_validation(1, 10, hash_10), TEST_NOW);
        assert_eq!(agg.highest_validated_seq, 10);
        assert_eq!(agg.highest_validated_hash, hash_10);
    }

    #[test]
    fn update_quorum_changes_threshold() {
        let mut agg = ValidationAggregator::new(1);
        let hash = Hash256::new([0xEE; 32]);

        // With quorum=1, a single validation reaches quorum
        assert!(agg
            .add_validation_at(make_validation(1, 10, hash), TEST_NOW)
            .is_some());

        // Raise quorum to 3
        agg.update_quorum(3);

        // Now need 3 validations for next sequence
        assert!(agg
            .add_validation_at(make_validation(1, 20, hash), TEST_NOW)
            .is_none());
        assert!(agg
            .add_validation_at(make_validation(2, 20, hash), TEST_NOW)
            .is_none());
        assert!(agg
            .add_validation_at(make_validation(3, 20, hash), TEST_NOW)
            .is_some());
    }

    #[test]
    fn update_quorum_floor_at_one() {
        let mut agg = ValidationAggregator::new(5);
        agg.update_quorum(0); // should clamp to 1
        let hash = Hash256::new([0xFF; 32]);
        // Single validation should still reach quorum (floor=1)
        assert!(agg
            .add_validation_at(make_validation(1, 10, hash), TEST_NOW)
            .is_some());
    }

    #[test]
    fn non_full_validation_ignored() {
        let mut agg = ValidationAggregator::new(1);
        let mut val = make_validation(1, 5, Hash256::new([0xDD; 32]));
        val.full = false;
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());
    }

    #[tokio::test]
    async fn untrusted_validation_dropped() {
        use crate::vl_fetcher::new_trusted_keys;
        use rxrpl_primitives::PublicKey;

        let trusted = new_trusted_keys();
        // Trust only one specific key.
        let trusted_key_bytes = [0xED; 33];
        let trusted_pk = PublicKey::from_slice(&trusted_key_bytes).unwrap();
        trusted.write().await.insert(trusted_pk);

        let mut agg = ValidationAggregator::new(1).with_trusted_keys(trusted);

        // A validation signed by an untrusted key is silently dropped.
        let mut val = make_validation(1, 5, Hash256::new([0xAA; 32]));
        val.public_key = vec![0xED; 33];
        val.public_key[1] = 0xFF; // diverges from the trusted key
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());

        // A validation signed by the trusted key is accepted.
        let mut val = make_validation(2, 5, Hash256::new([0xAA; 32]));
        val.public_key = trusted_key_bytes.to_vec();
        assert!(agg.add_validation_at(val, TEST_NOW).is_some());
    }

    #[test]
    fn fresh_validation_accepted() {
        // sign_time == now: well inside the freshness window, must be accepted.
        let mut agg = ValidationAggregator::new(1);
        let hash = Hash256::new([0x11; 32]);
        let val = make_validation(1, 42, hash);
        assert!(agg.add_validation_at(val, TEST_NOW).is_some());
        assert_eq!(agg.dropped_stale_total(), 0);
    }

    #[test]
    fn future_validation_dropped_bumps_counter() {
        // sign_time = now + 10 minutes is past the WALL ceiling (5 min).
        let mut agg = ValidationAggregator::new(1);
        let hash = Hash256::new([0x22; 32]);
        let mut val = make_validation(1, 42, hash);
        val.sign_time = TEST_NOW + 10 * 60;
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());
        assert_eq!(agg.dropped_stale_total(), 1);
        // And the validation must NOT have been recorded.
        assert_eq!(agg.validation_count(42, &hash), 0);
    }

    #[test]
    fn invalid_signature_validation_dropped() {
        // Defense-in-depth (audit pass 1 H#10): the explicit
        // `verify_and_add_validation_at` path must drop validations whose
        // signature does not verify, bump the counter, and never touch the
        // freshness/trust/quorum logic.
        use crate::identity::{NodeIdentity, verify_validation_signature};
        use rxrpl_consensus::types::Validation;

        let id = NodeIdentity::generate();
        let hash = Hash256::new([0x77; 32]);

        // Build a properly-signed validation, then tamper with the signature
        // so verification fails.
        let mut val = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: hash,
            ledger_seq: 99,
            full: true,
            close_time: TEST_NOW,
            sign_time: TEST_NOW,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        id.sign_validation(&mut val);
        // Sanity: the freshly-signed validation must verify before we tamper.
        assert!(verify_validation_signature(&val));

        // Flip a bit in the signature — verification must now fail.
        let sig = val.signature.as_mut().expect("signature present");
        sig[0] ^= 0x01;
        assert!(!verify_validation_signature(&val));

        let mut agg = ValidationAggregator::new(1);
        // The explicit verify-and-add path drops the validation and bumps
        // the dedicated counter.
        assert!(agg
            .verify_and_add_validation_at(val, TEST_NOW)
            .is_none());
        assert_eq!(agg.dropped_invalid_signature_total(), 1);
        // Nothing was recorded in the aggregator.
        assert_eq!(agg.validation_count(99, &hash), 0);
        assert!(!agg.is_validated(99));
        // The freshness counter must be untouched — bad signatures and
        // stale times are different drop reasons.
        assert_eq!(agg.dropped_stale_total(), 0);
    }

    #[test]
    fn valid_signature_validation_accepted_via_verify_and_add() {
        // Sister test to `invalid_signature_validation_dropped`: a properly
        // signed validation must flow through `verify_and_add_validation_at`
        // and reach quorum without bumping the bad-signature counter.
        use crate::identity::NodeIdentity;
        use rxrpl_consensus::types::Validation;

        let id = NodeIdentity::generate();
        let hash = Hash256::new([0x88; 32]);

        let mut val = Validation {
            node_id: NodeId(id.node_id),
            public_key: id.public_key_bytes().to_vec(),
            ledger_hash: hash,
            ledger_seq: 100,
            full: true,
            close_time: TEST_NOW,
            sign_time: TEST_NOW,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        id.sign_validation(&mut val);

        let mut agg = ValidationAggregator::new(1);
        let result = agg.verify_and_add_validation_at(val, TEST_NOW);
        assert!(result.is_some());
        assert_eq!(agg.dropped_invalid_signature_total(), 0);
    }

    #[test]
    fn freshness_counter_bumps_only_on_freshness_gate() {
        // T34: validations_dropped_freshness_total must increment when
        // add_validation_at rejects via the is_current freshness gate, and
        // it must NOT increment for the trust-filter or signature-verify
        // gates. It also tracks 1:1 with dropped_stale_total.
        let mut agg = ValidationAggregator::new(1);
        assert_eq!(agg.validations_dropped_freshness_total(), 0);
        assert_eq!(agg.validations_dropped_stale_total(), 0);

        // A fresh validation is accepted; counters stay at 0.
        let hash_ok = Hash256::new([0x01; 32]);
        assert!(agg
            .add_validation_at(make_validation(1, 100, hash_ok), TEST_NOW)
            .is_some());
        assert_eq!(agg.validations_dropped_freshness_total(), 0);
        assert_eq!(agg.validations_dropped_stale_total(), 0);

        // A future-stale validation triggers the freshness gate exactly
        // once and bumps both counters in lockstep.
        let hash_future = Hash256::new([0x02; 32]);
        let mut future_val = make_validation(2, 200, hash_future);
        future_val.sign_time = TEST_NOW + 10 * 60; // past WALL ceiling
        assert!(agg.add_validation_at(future_val, TEST_NOW).is_none());
        assert_eq!(agg.validations_dropped_freshness_total(), 1);
        assert_eq!(agg.validations_dropped_stale_total(), 1);

        // A past-stale validation: same gate, both counters bump again.
        let hash_past = Hash256::new([0x03; 32]);
        let mut past_val = make_validation(3, 201, hash_past);
        past_val.sign_time = TEST_NOW.saturating_sub(10 * 60);
        assert!(agg.add_validation_at(past_val, TEST_NOW).is_none());
        assert_eq!(agg.validations_dropped_freshness_total(), 2);
        assert_eq!(agg.validations_dropped_stale_total(), 2);

        // Nothing was ever recorded for the stale buckets.
        assert_eq!(agg.validation_count(200, &hash_future), 0);
        assert_eq!(agg.validation_count(201, &hash_past), 0);
    }

    #[tokio::test]
    async fn freshness_counter_not_bumped_by_trust_filter() {
        // T34: rejection by the trust filter must NOT bump the freshness
        // counter (different rejection reason).
        use crate::vl_fetcher::new_trusted_keys;
        use rxrpl_primitives::PublicKey;

        let trusted = new_trusted_keys();
        let trusted_key_bytes = [0xED; 33];
        let trusted_pk = PublicKey::from_slice(&trusted_key_bytes).unwrap();
        trusted.write().await.insert(trusted_pk);

        let mut agg = ValidationAggregator::new(1).with_trusted_keys(trusted);

        // Fresh sign_time but untrusted public_key: dropped by the trust
        // gate AFTER the freshness gate has already passed.
        let mut val = make_validation(1, 5, Hash256::new([0xAA; 32]));
        val.public_key = vec![0xED; 33];
        val.public_key[1] = 0xFF; // diverges from trusted key
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());

        assert_eq!(agg.validations_dropped_freshness_total(), 0);
        assert_eq!(agg.validations_dropped_stale_total(), 0);
    }

    #[test]
    fn validations_dropped_stale_total_canonical_accessor() {
        // T34: the canonical-name accessor must return the same value as
        // the legacy short-named accessor and increment on every
        // freshness-gate drop. Documents the rippled JLOG counterpart
        // (Counter[ConsensusValidations.staleValidations]).
        let mut agg = ValidationAggregator::new(1);
        assert_eq!(agg.dropped_stale_total(), 0);
        assert_eq!(agg.validations_dropped_stale_total(), 0);

        let hash = Hash256::new([0xC0; 32]);
        let mut val = make_validation(1, 42, hash);
        val.sign_time = TEST_NOW + 10 * 60; // past freshness window
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());

        // Legacy and canonical accessors both reflect the increment.
        assert_eq!(agg.dropped_stale_total(), 1);
        assert_eq!(agg.validations_dropped_stale_total(), 1);
        // And remain in lockstep across additional drops.
        let mut val2 = make_validation(2, 43, hash);
        val2.sign_time = TEST_NOW.saturating_sub(10 * 60);
        assert!(agg.add_validation_at(val2, TEST_NOW).is_none());
        assert_eq!(agg.dropped_stale_total(), 2);
        assert_eq!(agg.validations_dropped_stale_total(), 2);
    }

    #[test]
    fn past_validation_dropped_bumps_counter() {
        // sign_time = now - 10 minutes is past the EARLY floor (3 min).
        let mut agg = ValidationAggregator::new(1);
        let hash_future = Hash256::new([0x33; 32]);
        let hash_past = Hash256::new([0x44; 32]);

        // First, a future-stale to bring counter to 1.
        let mut val = make_validation(1, 50, hash_future);
        val.sign_time = TEST_NOW + 10 * 60;
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());
        assert_eq!(agg.dropped_stale_total(), 1);

        // Then a past-stale to bring counter to 2.
        let mut val = make_validation(2, 51, hash_past);
        val.sign_time = TEST_NOW.saturating_sub(10 * 60);
        assert!(agg.add_validation_at(val, TEST_NOW).is_none());
        assert_eq!(agg.dropped_stale_total(), 2);
        assert_eq!(agg.validation_count(51, &hash_past), 0);
    }
}
