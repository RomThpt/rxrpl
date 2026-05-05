use std::collections::{HashMap, HashSet};

use crate::types::NodeId;

/// Flag ledger interval: every 256 ledgers.
pub const FLAG_LEDGER_INTERVAL: u32 = 256;

/// Minimum validation ratio to be considered reliable (>50%).
/// A validator must send validations for more than this fraction
/// of ledgers in the window to avoid demotion.
const RELIABILITY_THRESHOLD: f64 = 0.5;

/// Maximum fraction of the UNL that can be placed on the negative UNL
/// simultaneously. The XRPL caps this to prevent quorum collapse.
const MAX_NEGATIVE_UNL_FRACTION: f64 = 0.25;

/// A UNLModify action to be emitted as a pseudo-transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegativeUnlChange {
    /// The validator's public key (hex-encoded master key).
    pub validator_key: String,
    /// Whether to disable (true) or re-enable (false) the validator.
    pub disable: bool,
    /// The ledger sequence at which this change is generated.
    pub ledger_seq: u32,
}

/// Per-validator tracking entry in the sliding window.
#[derive(Clone, Debug, Default)]
struct ValidatorRecord {
    /// Number of validations received in the current window.
    validations_received: u32,
}

/// Tracks validation receipts across a sliding window of flag ledger intervals
/// and generates UNLModify pseudo-transactions for unreliable validators.
///
/// The tracker operates on a per-flag-ledger-interval basis:
/// - Each ledger, call `record_validation` for every trusted validator
///   that sent a validation.
/// - At each flag ledger (ledger_seq % 256 == 0), call `evaluate`
///   to get the set of UNLModify pseudo-transactions.
#[derive(Clone, Debug)]
pub struct NegativeUnlTracker {
    /// Number of ledgers observed in the current window.
    ledgers_in_window: u32,
    /// Per-validator validation counts for the current window.
    current_window: HashMap<NodeId, ValidatorRecord>,
    /// Previous window results for comparison (used for re-enable checks).
    previous_window: HashMap<NodeId, ValidatorRecord>,
    /// Mapping from NodeId to hex-encoded master public key.
    node_to_key: HashMap<NodeId, String>,
    /// The set of validators currently on the negative UNL
    /// (tracked locally for cap enforcement).
    currently_disabled: HashSet<NodeId>,
}

impl NegativeUnlTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self {
            ledgers_in_window: 0,
            current_window: HashMap::new(),
            previous_window: HashMap::new(),
            node_to_key: HashMap::new(),
            currently_disabled: HashSet::new(),
        }
    }

    /// Register a validator's public key mapping.
    /// Must be called so the tracker can emit hex keys in UNLModify pseudo-txs.
    pub fn register_validator(&mut self, node_id: NodeId, hex_public_key: String) {
        self.node_to_key.insert(node_id, hex_public_key);
    }

    /// Record that a specific ledger has been closed.
    /// Call this once per ledger to advance the window counter.
    pub fn on_ledger_close(&mut self) {
        self.ledgers_in_window += 1;
    }

    /// Record a validation receipt from a trusted validator for the current ledger.
    pub fn record_validation(&mut self, node_id: NodeId) {
        self.current_window
            .entry(node_id)
            .or_default()
            .validations_received += 1;
    }

    /// Mark a validator as currently disabled (on the negative UNL).
    /// Called when loading existing negative UNL state at startup or after
    /// applying UNLModify transactions.
    pub fn mark_disabled(&mut self, node_id: NodeId) {
        self.currently_disabled.insert(node_id);
    }

    /// Mark a validator as re-enabled (removed from negative UNL).
    pub fn mark_enabled(&mut self, node_id: &NodeId) {
        self.currently_disabled.remove(node_id);
    }

    /// Check if a given ledger sequence is a flag ledger.
    pub fn is_flag_ledger(ledger_seq: u32) -> bool {
        ledger_seq > 0 && ledger_seq % FLAG_LEDGER_INTERVAL == 0
    }

    /// Evaluate the current window and produce UNLModify changes.
    ///
    /// Should be called at each flag ledger (every 256 ledgers).
    /// After calling, the window is rotated: the current window becomes
    /// the previous window, and a fresh window starts.
    ///
    /// `trusted_set` is the full set of trusted validators (not excluding nUNL).
    /// `ledger_seq` is the current flag ledger sequence number.
    pub fn evaluate(
        &mut self,
        trusted_set: &HashSet<NodeId>,
        ledger_seq: u32,
    ) -> Vec<NegativeUnlChange> {
        let mut changes = Vec::new();

        if self.ledgers_in_window == 0 {
            self.rotate_window();
            return changes;
        }

        let max_disabled = max_negative_unl_size(trusted_set.len());

        // Phase 1: Check for validators to re-enable.
        // A validator on the negative UNL that has validated >50% of the
        // window should be re-enabled.
        let disabled_snapshot: Vec<NodeId> = self.currently_disabled.iter().copied().collect();
        for node_id in &disabled_snapshot {
            if !trusted_set.contains(node_id) {
                continue;
            }
            let ratio = self.validation_ratio(node_id);
            if ratio > RELIABILITY_THRESHOLD {
                if let Some(key) = self.node_to_key.get(node_id) {
                    changes.push(NegativeUnlChange {
                        validator_key: key.clone(),
                        disable: false,
                        ledger_seq,
                    });
                    self.currently_disabled.remove(node_id);
                }
            }
        }

        // Phase 2: Check for validators to disable.
        // Only consider validators not already on the negative UNL.
        // Respect the max cap.
        for node_id in trusted_set {
            if self.currently_disabled.contains(node_id) {
                continue;
            }
            if self.currently_disabled.len() >= max_disabled {
                break;
            }

            let ratio = self.validation_ratio(node_id);
            if ratio <= RELIABILITY_THRESHOLD {
                if let Some(key) = self.node_to_key.get(node_id) {
                    changes.push(NegativeUnlChange {
                        validator_key: key.clone(),
                        disable: true,
                        ledger_seq,
                    });
                    self.currently_disabled.insert(*node_id);
                }
            }
        }

        self.rotate_window();
        changes
    }

    /// Get the validation ratio for a validator in the current window.
    fn validation_ratio(&self, node_id: &NodeId) -> f64 {
        if self.ledgers_in_window == 0 {
            return 0.0;
        }
        let received = self
            .current_window
            .get(node_id)
            .map(|r| r.validations_received)
            .unwrap_or(0);
        received as f64 / self.ledgers_in_window as f64
    }

    /// Rotate: current window becomes previous, start a fresh window.
    fn rotate_window(&mut self) {
        self.previous_window = std::mem::take(&mut self.current_window);
        self.ledgers_in_window = 0;
    }

    /// Get the number of currently disabled validators.
    pub fn disabled_count(&self) -> usize {
        self.currently_disabled.len()
    }

    /// Get the set of currently disabled validators.
    pub fn disabled_set(&self) -> &HashSet<NodeId> {
        &self.currently_disabled
    }
}

impl Default for NegativeUnlTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the maximum number of validators that can be on the negative UNL.
/// Capped at 25% of total trusted set size (rounded down), minimum 0.
fn max_negative_unl_size(trusted_count: usize) -> usize {
    ((trusted_count as f64) * MAX_NEGATIVE_UNL_FRACTION).floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn node(id: u8) -> NodeId {
        NodeId(Hash256::new([id; 32]))
    }

    fn make_trusted_set(ids: &[u8]) -> HashSet<NodeId> {
        ids.iter().map(|&id| node(id)).collect()
    }

    fn setup_tracker(validator_ids: &[u8]) -> NegativeUnlTracker {
        let mut tracker = NegativeUnlTracker::new();
        for &id in validator_ids {
            tracker.register_validator(node(id), format!("ED{:0>64}", hex::encode([id; 32])));
        }
        tracker
    }

    #[test]
    fn is_flag_ledger_checks() {
        assert!(!NegativeUnlTracker::is_flag_ledger(0));
        assert!(!NegativeUnlTracker::is_flag_ledger(1));
        assert!(!NegativeUnlTracker::is_flag_ledger(255));
        assert!(NegativeUnlTracker::is_flag_ledger(256));
        assert!(!NegativeUnlTracker::is_flag_ledger(257));
        assert!(NegativeUnlTracker::is_flag_ledger(512));
        assert!(NegativeUnlTracker::is_flag_ledger(768));
    }

    #[test]
    fn no_changes_on_empty_window() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);
        let changes = tracker.evaluate(&trusted, 256);
        assert!(changes.is_empty());
    }

    #[test]
    fn reliable_validators_not_demoted() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        // Simulate 256 ledgers with all validators validating every ledger
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=5 {
                tracker.record_validation(node(id));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert!(changes.is_empty());
    }

    #[test]
    fn unreliable_validator_demoted() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        // Simulate 256 ledgers: validator 5 only validates 100/256 (~39%)
        for i in 0..256u32 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            if i < 100 {
                tracker.record_validation(node(5));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].disable);
        assert!(changes[0].validator_key.contains(&hex::encode([5u8; 32])));
    }

    #[test]
    fn validator_exactly_at_threshold_demoted() {
        // 50% exactly should be demoted (threshold is >50%, not >=50%)
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        for i in 0..256u32 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            // Exactly 128/256 = 50%
            if i < 128 {
                tracker.record_validation(node(5));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].disable);
    }

    #[test]
    fn validator_above_threshold_not_demoted() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        for i in 0..256u32 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            // 129/256 ~= 50.4% which is > 50%
            if i < 129 {
                tracker.record_validation(node(5));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert!(changes.is_empty());
    }

    #[test]
    fn disabled_validator_re_enabled_when_reliable() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        // Mark validator 5 as already disabled
        tracker.mark_disabled(node(5));

        // Simulate a window where validator 5 comes back (validates 200/256 ~78%)
        for i in 0..256u32 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            if i < 200 {
                tracker.record_validation(node(5));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].disable); // re-enable
        assert!(changes[0].validator_key.contains(&hex::encode([5u8; 32])));
    }

    #[test]
    fn disabled_validator_stays_disabled_if_still_unreliable() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        tracker.mark_disabled(node(5));

        // Validator 5 still only validates 50/256 ~= 19.5%
        for i in 0..256u32 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            if i < 50 {
                tracker.record_validation(node(5));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        // No changes: already disabled and still unreliable
        assert!(changes.is_empty());
    }

    #[test]
    fn max_negative_unl_cap_enforced() {
        // With 8 validators, max disabled = floor(8 * 0.25) = 2
        let ids: Vec<u8> = (1..=8).collect();
        let mut tracker = setup_tracker(&ids);
        let trusted = make_trusted_set(&ids);

        // Validators 6, 7, 8 all miss everything
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=5 {
                tracker.record_validation(node(id));
            }
            // 6, 7, 8 validate 0 times
        }

        let changes = tracker.evaluate(&trusted, 256);
        // Only 2 should be disabled (cap = floor(8*0.25) = 2)
        let disable_count = changes.iter().filter(|c| c.disable).count();
        assert_eq!(disable_count, 2);
    }

    #[test]
    fn window_rotates_after_evaluate() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        // First window: all validate
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=5 {
                tracker.record_validation(node(id));
            }
        }

        tracker.evaluate(&trusted, 256);

        // Window should be reset
        assert_eq!(tracker.ledgers_in_window, 0);

        // Second window: validator 5 goes silent
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
        }

        let changes = tracker.evaluate(&trusted, 512);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].disable);
    }

    #[test]
    fn max_negative_unl_size_calculation() {
        assert_eq!(max_negative_unl_size(0), 0);
        assert_eq!(max_negative_unl_size(1), 0);
        assert_eq!(max_negative_unl_size(3), 0);
        assert_eq!(max_negative_unl_size(4), 1);
        assert_eq!(max_negative_unl_size(5), 1);
        assert_eq!(max_negative_unl_size(8), 2);
        assert_eq!(max_negative_unl_size(10), 2);
        assert_eq!(max_negative_unl_size(20), 5);
    }

    #[test]
    fn unregistered_validator_not_emitted() {
        // If a validator has no registered key, no change is emitted
        let mut tracker = NegativeUnlTracker::new();
        // Only register validators 1-4, not 5
        for id in 1..=4 {
            tracker.register_validator(node(id), format!("ED{:0>64}", hex::encode([id; 32])));
        }

        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
            // node(5) never validates and has no registered key
        }

        let changes = tracker.evaluate(&trusted, 256);
        // node(5) would be demoted but has no key -> no change emitted
        assert!(changes.is_empty());
    }

    #[test]
    fn completely_absent_validator_demoted() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        // Validator 5 never validates at all
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
        }

        let changes = tracker.evaluate(&trusted, 256);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].disable);
    }

    #[test]
    fn re_enable_before_disable_in_same_evaluation() {
        // If a previously disabled validator becomes reliable,
        // it gets re-enabled, freeing a slot for a new demotion.
        let ids: Vec<u8> = (1..=8).collect();
        let mut tracker = setup_tracker(&ids);
        let trusted = make_trusted_set(&ids);

        // Validator 6 was previously disabled
        tracker.mark_disabled(node(6));

        // This window: 6 is reliable again, but 7 and 8 are absent
        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=6 {
                tracker.record_validation(node(id));
            }
            // 7, 8 are absent
        }

        let changes = tracker.evaluate(&trusted, 256);

        // Should have: re-enable node(6), disable node(7) and/or node(8)
        let re_enables: Vec<_> = changes.iter().filter(|c| !c.disable).collect();
        let disables: Vec<_> = changes.iter().filter(|c| c.disable).collect();

        assert_eq!(re_enables.len(), 1);
        assert!(
            re_enables[0]
                .validator_key
                .contains(&hex::encode([6u8; 32]))
        );
        // Cap is floor(8*0.25) = 2, was 1 disabled (node 6), re-enabled -> 0,
        // so up to 2 new disables
        assert!(disables.len() <= 2);
        assert!(!disables.is_empty());
    }

    #[test]
    fn small_unl_no_demotion_possible() {
        // With 3 validators, max disabled = floor(3 * 0.25) = 0
        // No demotion should occur
        let mut tracker = setup_tracker(&[1, 2, 3]);
        let trusted = make_trusted_set(&[1, 2, 3]);

        for _ in 0..256 {
            tracker.on_ledger_close();
            tracker.record_validation(node(1));
            tracker.record_validation(node(2));
            // node(3) absent
        }

        let changes = tracker.evaluate(&trusted, 256);
        // Cap is 0 -> no disables possible
        let disables: Vec<_> = changes.iter().filter(|c| c.disable).collect();
        assert!(disables.is_empty());
    }

    #[test]
    fn mark_enabled_removes_from_disabled() {
        let mut tracker = NegativeUnlTracker::new();
        tracker.mark_disabled(node(1));
        assert_eq!(tracker.disabled_count(), 1);
        tracker.mark_enabled(&node(1));
        assert_eq!(tracker.disabled_count(), 0);
    }

    #[test]
    fn ledger_seq_included_in_changes() {
        let mut tracker = setup_tracker(&[1, 2, 3, 4, 5]);
        let trusted = make_trusted_set(&[1, 2, 3, 4, 5]);

        for _ in 0..256 {
            tracker.on_ledger_close();
            for id in 1..=4 {
                tracker.record_validation(node(id));
            }
        }

        let changes = tracker.evaluate(&trusted, 768);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].ledger_seq, 768);
    }
}
