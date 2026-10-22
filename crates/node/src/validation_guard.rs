//! Self-validation safety guards (validator-readiness M0).
//!
//! A trusted validator that signs two conflicting FULL validations for the
//! same ledger sequence (different ledger hashes) is treated as *equivocating*
//! by the rest of the network and dropped. rxrpl produces FULL validations
//! from two independent places — the consensus-close path and the
//! catchup-adopt path — so a sync/consensus race could emit two conflicting
//! validations for one sequence. This guard records the highest `(seq, hash)`
//! this node has self-validated and refuses to emit a second, *conflicting*
//! FULL validation. Re-signing the SAME `(seq, hash)` stays idempotent.
//!
//! It additionally enforces a monotonic `SigningTime` floor so successive
//! validations never carry a non-increasing timestamp.
//!
//! rippled refs:
//!   - `RCLConsensus::Adaptor::validate` (`RCLConsensus.cpp`): the
//!     `validationTime <= lastValidationTime_ ? lastValidationTime_ + 1s`
//!     monotonic SigningTime floor.
//!   - `RCLValidations` / `Validations::add`: rejects a validator's second,
//!     conflicting validation for a sequence it already validated.

use rxrpl_primitives::Hash256;

/// In-memory guard state for this node's own FULL validations.
///
/// Held alongside the consensus loop where validations are produced. In-memory
/// persistence is sufficient for now: a restart re-derives the high-water mark
/// from the validated ledger before it resumes validating.
#[derive(Debug, Default, Clone)]
pub struct SelfValidationGuard {
    /// Highest ledger sequence this node has emitted a FULL validation for.
    highest_seq: u32,
    /// The ledger hash validated at `highest_seq`.
    highest_hash: Hash256,
    /// True once at least one FULL validation has been recorded.
    seeded: bool,
    /// `SigningTime` of the most recent emitted validation (Ripple-epoch secs).
    last_signing_time: u32,
}

impl SelfValidationGuard {
    /// Decide whether a FULL validation for `(seq, hash)` may be signed and
    /// broadcast.
    ///
    /// Refuses (returns `false`) on a genuine equivocation / regression:
    ///   * `seq < highest_seq` — would validate an older ledger after a newer
    ///     one (a sync/consensus race walking backwards), and
    ///   * `seq == highest_seq` with a DIFFERENT hash — a second, conflicting
    ///     FULL validation for the same sequence (a double-sign).
    ///
    /// Re-signing the SAME `(seq, hash)` is idempotent and allowed, as is any
    /// strictly-newer sequence.
    pub fn may_validate(&self, seq: u32, hash: Hash256) -> bool {
        if !self.seeded || seq > self.highest_seq {
            return true;
        }
        if seq == self.highest_seq {
            return hash == self.highest_hash;
        }
        // seq < highest_seq
        false
    }

    /// Record a FULL validation that was actually emitted, advancing the
    /// equivocation high-water mark. Only moves forward; replaying the same or
    /// an older sequence does not rewind the guard.
    pub fn record_validation(&mut self, seq: u32, hash: Hash256) {
        if !self.seeded || seq > self.highest_seq {
            self.highest_seq = seq;
            self.highest_hash = hash;
            self.seeded = true;
        }
    }

    /// Monotonic `SigningTime` floor: `max(base_time, last_signing_time + 1)`.
    ///
    /// Mirrors `RCLConsensus::Adaptor::validate`'s
    /// `validationTime = max(now, lastValidationTime_ + 1s)`. `base_time` is the
    /// path-specific base (the ledger close time on the consensus-close path,
    /// or the wall-clock NetClock on the catchup-adopt path); the floor only
    /// ever *raises* it, never lowers it.
    pub fn floor_signing_time(&self, base_time: u32) -> u32 {
        base_time.max(self.last_signing_time.saturating_add(1))
    }

    /// Record the `SigningTime` actually used so the next floor stays monotonic.
    /// Keeps the maximum in case validations are produced out of order.
    pub fn record_signing_time(&mut self, signing_time: u32) {
        if signing_time > self.last_signing_time {
            self.last_signing_time = signing_time;
        }
    }

    /// Highest self-validated sequence (for diagnostics / logging).
    pub fn highest_seq(&self) -> u32 {
        self.highest_seq
    }

    /// Last `SigningTime` emitted (for diagnostics / logging).
    pub fn last_signing_time(&self) -> u32 {
        self.last_signing_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash256 {
        Hash256::new([b; 32])
    }

    #[test]
    fn fresh_guard_allows_anything() {
        let g = SelfValidationGuard::default();
        assert!(g.may_validate(100, h(1)));
        assert!(g.may_validate(1, h(9)));
    }

    #[test]
    fn allows_resign_same_seq_same_hash() {
        let mut g = SelfValidationGuard::default();
        g.record_validation(100, h(1));
        // Idempotent re-sign of the identical (seq, hash) is fine.
        assert!(g.may_validate(100, h(1)));
    }

    #[test]
    fn refuses_conflicting_hash_same_seq() {
        let mut g = SelfValidationGuard::default();
        g.record_validation(100, h(1));
        // Same seq, DIFFERENT hash == double-sign -> refused.
        assert!(!g.may_validate(100, h(2)));
    }

    #[test]
    fn refuses_older_seq() {
        let mut g = SelfValidationGuard::default();
        g.record_validation(100, h(1));
        // Walking backwards to an older seq -> refused.
        assert!(!g.may_validate(99, h(3)));
        assert!(!g.may_validate(99, h(1)));
    }

    #[test]
    fn allows_newer_seq() {
        let mut g = SelfValidationGuard::default();
        g.record_validation(100, h(1));
        assert!(g.may_validate(101, h(2)));
    }

    #[test]
    fn record_only_moves_forward() {
        let mut g = SelfValidationGuard::default();
        g.record_validation(100, h(1));
        // Recording an older seq must not rewind the high-water mark.
        g.record_validation(99, h(3));
        assert_eq!(g.highest_seq(), 100);
        assert!(!g.may_validate(99, h(3)));
        // And the stored hash for seq 100 is unchanged.
        assert!(g.may_validate(100, h(1)));
        assert!(!g.may_validate(100, h(9)));
    }

    #[test]
    fn signing_time_uses_base_when_above_floor() {
        let g = SelfValidationGuard::default();
        // Nothing recorded yet: floor is last+1 = 1, base wins.
        assert_eq!(g.floor_signing_time(1_000), 1_000);
    }

    #[test]
    fn signing_time_floored_when_base_not_increasing() {
        let mut g = SelfValidationGuard::default();
        g.record_signing_time(1_000);
        // Same-second / regressing base is bumped to last + 1.
        assert_eq!(g.floor_signing_time(1_000), 1_001);
        assert_eq!(g.floor_signing_time(999), 1_001);
        // A strictly-greater base passes through untouched.
        assert_eq!(g.floor_signing_time(2_000), 2_000);
    }

    #[test]
    fn signing_time_record_keeps_max() {
        let mut g = SelfValidationGuard::default();
        g.record_signing_time(1_000);
        g.record_signing_time(500); // out-of-order, ignored
        assert_eq!(g.last_signing_time(), 1_000);
        assert_eq!(g.floor_signing_time(1_000), 1_001);
    }
}
