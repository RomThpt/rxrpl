use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::registry::FeatureRegistry;
use crate::rules::Rules;

/// Runtime amendment voting state.
///
/// Tracks which amendments have majority support and for how long,
/// enabling amendments that maintain majority for the required period.
#[derive(Debug)]
pub struct AmendmentTable {
    /// How many seconds of close-time an amendment must hold majority before
    /// activation (rippled's two-week window = 1,209,600 seconds).
    majority_time: u32,
    /// Map from amendment ID to voting state.
    state: HashMap<Hash256, AmendmentState>,
}

/// Voting state for a single amendment.
#[derive(Debug, Clone)]
struct AmendmentState {
    /// Whether this amendment is enabled.
    enabled: bool,
    /// Whether this validator supports this amendment.
    supported: bool,
    /// If currently has majority, the close-time (in seconds) when majority
    /// was first gained.
    majority_since: Option<u32>,
}

impl AmendmentTable {
    /// Create a new amendment table from a registry.
    ///
    /// `majority_time` is the number of seconds of close-time an amendment must
    /// hold majority before it becomes enabled (rippled's two-week window =
    /// 1,209,600 seconds).
    pub fn new(registry: &FeatureRegistry, majority_time: u32) -> Self {
        let mut state = HashMap::new();
        for feature in registry.all() {
            state.insert(
                feature.id,
                AmendmentState {
                    enabled: feature.retired,
                    supported: feature.default_vote,
                    majority_since: None,
                },
            );
        }
        Self {
            majority_time,
            state,
        }
    }

    /// Check if an amendment is enabled.
    pub fn is_enabled(&self, id: &Hash256) -> bool {
        self.state.get(id).is_some_and(|s| s.enabled)
    }

    /// Check if this validator supports (votes yes for) an amendment.
    pub fn is_supported(&self, id: &Hash256) -> bool {
        self.state.get(id).is_some_and(|s| s.supported)
    }

    /// Set whether this validator supports an amendment.
    pub fn set_supported(&mut self, id: &Hash256, supported: bool) {
        if let Some(state) = self.state.get_mut(id) {
            state.supported = supported;
        }
    }

    /// Enable an amendment directly (e.g., when loading from a validated ledger).
    pub fn enable(&mut self, id: &Hash256) {
        if let Some(state) = self.state.get_mut(id) {
            state.enabled = true;
        }
    }

    /// Check if an amendment currently has majority.
    pub fn has_majority(&self, id: &Hash256) -> bool {
        self.state
            .get(id)
            .is_some_and(|s| s.majority_since.is_some())
    }

    /// Record that an amendment has gained majority at a given close time.
    ///
    /// `close_time` is the close-time (in seconds) of the flag ledger on which
    /// majority was first observed; it is the reference point for the
    /// `majority_time` activation window.
    pub fn set_majority(&mut self, id: &Hash256, close_time: u32) {
        if let Some(state) = self.state.get_mut(id) {
            if state.majority_since.is_none() {
                state.majority_since = Some(close_time);
            }
        }
    }

    /// Record that an amendment has lost majority.
    pub fn clear_majority(&mut self, id: &Hash256) {
        if let Some(state) = self.state.get_mut(id) {
            state.majority_since = None;
        }
    }

    /// Check amendments for activation based on the current close time.
    ///
    /// An amendment activates once it has held majority for at least
    /// `majority_time` seconds of close-time (rippled's two-week window).
    /// Returns a list of newly activated amendment IDs, sorted by id so that
    /// multiple simultaneous activations emit their `EnableAmendment`
    /// pseudo-transactions in a deterministic order (avoids cross-impl
    /// ledger-hash divergence).
    pub fn check_activations(&mut self, current_close_time: u32) -> Vec<Hash256> {
        let mut activated = Vec::new();
        for (id, state) in &mut self.state {
            if state.enabled {
                continue;
            }
            if let Some(since) = state.majority_since {
                if current_close_time.saturating_sub(since) >= self.majority_time {
                    state.enabled = true;
                    activated.push(*id);
                }
            }
        }
        // Deterministic emission order (HashMap iteration is unordered).
        activated.sort();
        activated
    }

    /// Build a `Rules` snapshot from the current enabled state.
    pub fn build_rules(&self) -> Rules {
        let enabled: Vec<Hash256> = self
            .state
            .iter()
            .filter(|(_, s)| s.enabled)
            .map(|(id, _)| *id)
            .collect();
        Rules::from_enabled(enabled)
    }

    /// Get all amendment IDs this validator votes "yes" for.
    pub fn get_votes(&self) -> Vec<Hash256> {
        self.state
            .iter()
            .filter(|(_, s)| s.supported && !s.enabled)
            .map(|(id, _)| *id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;

    fn test_registry() -> FeatureRegistry {
        let mut reg = FeatureRegistry::new();
        reg.register(Feature::new("FeatureA", true));
        reg.register(Feature::new("FeatureB", false));
        reg.register(Feature::retired("RetiredFeature"));
        reg
    }

    #[test]
    fn retired_starts_enabled() {
        let reg = test_registry();
        let table = AmendmentTable::new(&reg, 100);
        let id = reg.id_for_name("RetiredFeature").unwrap();
        assert!(table.is_enabled(&id));
    }

    #[test]
    fn non_retired_starts_disabled() {
        let reg = test_registry();
        let table = AmendmentTable::new(&reg, 100);
        let id = reg.id_for_name("FeatureA").unwrap();
        assert!(!table.is_enabled(&id));
    }

    #[test]
    fn majority_and_activation() {
        let reg = test_registry();
        // majority_time is now close-time SECONDS, not a ledger count.
        let mut table = AmendmentTable::new(&reg, 100);
        let id = reg.id_for_name("FeatureA").unwrap();

        // Majority recorded at close_time = 1000.
        table.set_majority(&id, 1000);
        // 50 seconds later: still inside the window.
        assert!(table.check_activations(1050).is_empty());

        // 100 seconds later: window elapsed, amendment activates.
        let activated = table.check_activations(1100);
        assert!(activated.contains(&id));
        assert!(table.is_enabled(&id));
    }

    #[test]
    fn check_activations_uses_close_time_seconds() {
        // check_activations compares elapsed close-time SECONDS against
        // majority_time and only activates once `current - since >= majority_time`.
        let reg = test_registry();
        let majority_time: u32 = 1000;
        let mut table = AmendmentTable::new(&reg, majority_time);
        let id = reg.id_for_name("FeatureA").unwrap();

        let since: u32 = 5000;
        table.set_majority(&id, since);

        // One second short of the window: no activation.
        assert!(
            table
                .check_activations(since + majority_time - 1)
                .is_empty()
        );
        assert!(!table.is_enabled(&id));

        // Exactly at the window: activates.
        let activated = table.check_activations(since + majority_time);
        assert!(activated.contains(&id));
        assert!(table.is_enabled(&id));
    }

    #[test]
    fn check_activations_emits_sorted_ids() {
        // Multiple simultaneous activations must be returned in id-sorted order
        // for deterministic pseudo-tx emission across implementations.
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_a = reg.id_for_name("FeatureA").unwrap();
        let id_b = reg.id_for_name("FeatureB").unwrap();

        table.set_majority(&id_a, 1000);
        table.set_majority(&id_b, 1000);

        let activated = table.check_activations(1100);
        assert!(activated.contains(&id_a));
        assert!(activated.contains(&id_b));
        let mut expected = activated.clone();
        expected.sort();
        assert_eq!(activated, expected, "activations must be id-sorted");
    }

    #[test]
    fn lost_majority_resets() {
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id = reg.id_for_name("FeatureA").unwrap();

        table.set_majority(&id, 1000);
        table.clear_majority(&id);
        assert!(table.check_activations(1200).is_empty());
    }

    #[test]
    fn build_rules() {
        let reg = test_registry();
        let table = AmendmentTable::new(&reg, 100);
        let rules = table.build_rules();
        let retired_id = reg.id_for_name("RetiredFeature").unwrap();
        assert!(rules.enabled(&retired_id));
    }

    #[test]
    fn votes() {
        let reg = test_registry();
        let table = AmendmentTable::new(&reg, 100);
        let votes = table.get_votes();
        let id_a = reg.id_for_name("FeatureA").unwrap();
        let id_b = reg.id_for_name("FeatureB").unwrap();
        // FeatureA has default_vote=true, FeatureB has default_vote=false
        assert!(votes.contains(&id_a));
        assert!(!votes.contains(&id_b));
    }
}
