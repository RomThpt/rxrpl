use rxrpl_primitives::Hash256;

/// A protocol amendment feature.
#[derive(Clone, Debug)]
pub struct Feature {
    /// The feature's unique identifier (SHA-512-Half of its name).
    pub id: Hash256,
    /// Human-readable name (e.g., "FlowCross", "Hooks").
    pub name: String,
    /// Whether this amendment is retired (always enabled, cannot be voted out).
    pub retired: bool,
    /// Default vote for this amendment (yes/no).
    pub default_vote: bool,
}

impl Feature {
    /// Create a new feature with its ID derived from the name.
    pub fn new(name: impl Into<String>, default_vote: bool) -> Self {
        let name = name.into();
        let id = feature_id(&name);
        Self {
            id,
            name,
            retired: false,
            default_vote,
        }
    }

    /// Create a retired feature (always enabled).
    pub fn retired(name: impl Into<String>) -> Self {
        let name = name.into();
        let id = feature_id(&name);
        Self {
            id,
            name,
            retired: true,
            default_vote: true,
        }
    }
}

/// Derive a feature ID from its name using SHA-512-Half.
///
/// This matches rippled/goxrpld: `feature_id = SHA-512-Half(name)`.
pub fn feature_id(name: &str) -> Hash256 {
    rxrpl_crypto::sha512_half::sha512_half(&[name.as_bytes()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_id_deterministic() {
        let id1 = feature_id("FlowCross");
        let id2 = feature_id("FlowCross");
        assert_eq!(id1, id2);
        assert!(!id1.is_zero());
    }

    #[test]
    fn different_names_different_ids() {
        let id1 = feature_id("FlowCross");
        let id2 = feature_id("Hooks");
        assert_ne!(id1, id2);
    }

    #[test]
    fn feature_id_is_sha512_half() {
        // Verify the feature ID is the SHA-512-Half of the name string.
        let id = feature_id("MultiSignReserve");
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[b"MultiSignReserve"]);
        assert_eq!(id, expected);
        assert!(!id.is_zero());
    }

    #[test]
    fn new_feature() {
        let f = Feature::new("TestAmendment", true);
        assert_eq!(f.name, "TestAmendment");
        assert!(f.default_vote);
        assert!(!f.retired);
        assert_eq!(f.id, feature_id("TestAmendment"));
    }

    #[test]
    fn retired_feature() {
        let f = Feature::retired("OldAmendment");
        assert!(f.retired);
        assert!(f.default_vote);
    }
}
