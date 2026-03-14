use std::collections::HashSet;

use rxrpl_primitives::Hash256;

/// A snapshot of which amendments are active for a specific ledger.
///
/// Created when a ledger opens, passed by reference to all transactors.
/// Immutable once created -- represents the consensus rules for a ledger.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    enabled: HashSet<Hash256>,
}

impl Rules {
    /// Create rules with no amendments enabled.
    pub fn new() -> Self {
        Self {
            enabled: HashSet::new(),
        }
    }

    /// Create rules from a set of enabled amendment IDs.
    pub fn from_enabled(enabled: impl IntoIterator<Item = Hash256>) -> Self {
        Self {
            enabled: enabled.into_iter().collect(),
        }
    }

    /// Check if a specific amendment is enabled.
    pub fn enabled(&self, feature_id: &Hash256) -> bool {
        self.enabled.contains(feature_id)
    }

    /// Return the number of enabled amendments.
    pub fn count(&self) -> usize {
        self.enabled.len()
    }

    /// Return an iterator over all enabled amendment IDs.
    pub fn iter(&self) -> impl Iterator<Item = &Hash256> {
        self.enabled.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::feature_id;

    #[test]
    fn empty_rules() {
        let rules = Rules::new();
        let id = feature_id("SomeFeature");
        assert!(!rules.enabled(&id));
        assert_eq!(rules.count(), 0);
    }

    #[test]
    fn rules_with_enabled() {
        let id1 = feature_id("FeatureA");
        let id2 = feature_id("FeatureB");
        let id3 = feature_id("FeatureC");

        let rules = Rules::from_enabled([id1, id2]);
        assert!(rules.enabled(&id1));
        assert!(rules.enabled(&id2));
        assert!(!rules.enabled(&id3));
        assert_eq!(rules.count(), 2);
    }
}
