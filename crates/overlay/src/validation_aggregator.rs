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
    /// `validation_dropped_stale_total` metric.
    dropped_stale_total: AtomicU64,
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
        }
    }

    /// Number of validations dropped because they failed the freshness
    /// check (`is_current`). Monotonic across the lifetime of the aggregator.
    pub fn dropped_stale_total(&self) -> u64 {
        self.dropped_stale_total.load(Ordering::Relaxed)
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
