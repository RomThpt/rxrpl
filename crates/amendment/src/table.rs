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
    /// How many consecutive ledgers an amendment needs majority for activation.
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
    /// If currently has majority, the ledger sequence when majority was first gained.
    majority_since: Option<u32>,
}

impl AmendmentTable {
    /// Create a new amendment table from a registry.
    ///
    /// `majority_time` is the number of ledgers an amendment must hold majority
    /// before it becomes enabled (typically 14 days worth of ledgers).
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

    /// Record that an amendment has gained majority at a given ledger sequence.
    pub fn set_majority(&mut self, id: &Hash256, ledger_seq: u32) {
        if let Some(state) = self.state.get_mut(id) {
            if state.majority_since.is_none() {
                state.majority_since = Some(ledger_seq);
            }
        }
    }

    /// Record that an amendment has lost majority.
    pub fn clear_majority(&mut self, id: &Hash256) {
        if let Some(state) = self.state.get_mut(id) {
            state.majority_since = None;
        }
    }

    /// Check amendments for activation based on current ledger sequence.
    ///
    /// Returns a list of newly activated amendment IDs.
    pub fn check_activations(&mut self, current_seq: u32) -> Vec<Hash256> {
        let mut activated = Vec::new();
        for (id, state) in &mut self.state {
            if state.enabled {
                continue;
            }
            if let Some(since) = state.majority_since {
                if current_seq >= since + self.majority_time {
                    state.enabled = true;
                    activated.push(*id);
                }
            }
        }
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
        let mut table = AmendmentTable::new(&reg, 100);
        let id = reg.id_for_name("FeatureA").unwrap();

        table.set_majority(&id, 1000);
        assert!(table.check_activations(1050).is_empty());

        let activated = table.check_activations(1100);
        assert!(activated.contains(&id));
        assert!(table.is_enabled(&id));
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
