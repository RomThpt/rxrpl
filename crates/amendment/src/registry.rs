use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::feature::Feature;

/// Static registry of all known XRPL amendments.
///
/// Maps amendment IDs to their definitions. This is populated at startup
/// with all known amendments from the protocol.
#[derive(Debug)]
pub struct FeatureRegistry {
    by_id: HashMap<Hash256, Feature>,
    by_name: HashMap<String, Hash256>,
}

impl FeatureRegistry {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            by_name: HashMap::new(),
        }
    }

    /// Register a feature. Returns the feature ID.
    pub fn register(&mut self, feature: Feature) -> Hash256 {
        let id = feature.id;
        self.by_name.insert(feature.name.clone(), id);
        self.by_id.insert(id, feature);
        id
    }

    /// Look up a feature by its hash ID.
    pub fn get(&self, id: &Hash256) -> Option<&Feature> {
        self.by_id.get(id)
    }

    /// Look up a feature by name.
    pub fn get_by_name(&self, name: &str) -> Option<&Feature> {
        let id = self.by_name.get(name)?;
        self.by_id.get(id)
    }

    /// Get the feature ID for a name.
    pub fn id_for_name(&self, name: &str) -> Option<Hash256> {
        self.by_name.get(name).copied()
    }

    /// Return all registered features.
    pub fn all(&self) -> impl Iterator<Item = &Feature> {
        self.by_id.values()
    }

    /// Iterate the ids of every known amendment (the set this build can apply).
    /// Used by the amendment-blocked safety halt to detect an on-ledger
    /// amendment this node does not understand.
    pub fn known_ids(&self) -> impl Iterator<Item = &Hash256> {
        self.by_id.keys()
    }

    /// Number of registered features.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Create a registry pre-populated with known XRPL amendments.
    pub fn with_known_amendments() -> Self {
        let mut reg = Self::new();

        // Retired amendments (always enabled)
        for name in RETIRED_AMENDMENTS {
            reg.register(Feature::retired(*name));
        }

        // Active/voting amendments
        for (name, default_vote) in SUPPORTED_AMENDMENTS {
            reg.register(Feature::new(*name, *default_vote));
        }

        reg
    }
}

impl Default for FeatureRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Amendments that are retired (always active, cannot be voted out).
const RETIRED_AMENDMENTS: &[&str] = &[
    "MultiSign",
    "TrustSetAuth",
    "FeeEscalation",
    "PayChan",
    "CryptoConditions",
    "TickSize",
    "fix1368",
    "Escrow",
    "fix1373",
    "EnforceInvariants",
    "SortedDirectories",
    "fix1201",
    "fix1512",
    "fix1513",
    "fix1523",
    "fix1528",
    "DepositAuth",
    "Checks",
    "fix1571",
    "fix1543",
    "fix1623",
    "DepositPreauth",
    "fix1515",
    "fix1578",
    "MultiSignReserve",
    "fixTakerDryOfferRemoval",
    "fixMasterKeyAsRegularKey",
    "fixCheckThreading",
    "fixPayChanRecipientOwnerDir",
    "DeletableAccounts",
    "fixQualityUpperBound",
    "RequireFullyCanonicalSig",
    "fix1781",
    "HardenedValidations",
    "fixAmendmentMajorityCalc",
    "NegativeUNL",
    "TicketBatch",
    "FlowSortStrands",
    "fixSTAmountCanonicalize",
    "fixRmSmallIncreasedQOffers",
    "CheckCashMakesTrustLine",
    "NonFungibleTokensV1_1",
    "fixTrustLinesToSelf",
    "fixRemoveNFTokenAutoTrustLine",
    "ImmediateOfferKilled",
    "DisallowIncoming",
    "XRPFees",
    "fixUniversalNumber",
    "fixNonFungibleTokensV1_2",
    "fixNFTokenRemint",
    "fixReducedOffersV1",
    "Clawback",
    "AMM",
    "XChainBridge",
    "fixDisallowIncomingV1",
    "DID",
    "fixFillOrKill",
    "fixNFTokenReserve",
    "fixInnerObjTemplate",
    "fixAMMOverflowOffer",
    "PriceOracle",
    "fixEmptyDID",
    "fixXChainRewardRounding",
    "fixPreviousTxnID",
    "fixAMMv1_1",
    "NFTokenMintOffer",
    "fixReducedOffersV2",
    "fixEnforceNFTokenTrustline",
    "fixAMMv1_2",
    "fixAMMv1_3",
    "fixFrozenLPTokenTransfer",
    "fixEnforceNFTokenTrustlineV2",
    "fixInnerObjTemplate2",
    "fixInvalidTxFlags",
    "fixNFTokenPageLinks",
    "ExpandedSignerList",
];

/// Amendments that are supported but may not yet be enabled on mainnet.
///
/// Every name here must be a real rippled amendment (see rippled's
/// `features.macro`); the amendment id is `SHA512Half(name)`, so a misspelled
/// or invented name computes an id that matches no on-ledger amendment and can
/// never be gated or voted. The phantom names `TokenKeg` and `InvariantsV1_1`
/// (no rippled counterpart) were removed, and `Delegate` was corrected to its
/// real name `PermissionDelegationV1_1`.
const SUPPORTED_AMENDMENTS: &[(&str, bool)] = &[
    ("FlowCross", true),
    ("Flow", true),
    ("OwnerPaysFee", false),
    ("fixNFTokenNegOffer", true),
    ("fixNFTokenDirV1", true),
    ("MPTokensV1", true),
    ("Credentials", true),
    ("AMMClawback", true),
    ("PermissionedDomains", true),
    ("DeepFreeze", false),
    ("SingleAssetVault", false),
    ("Batch", false),
    // Real rippled name for the granular-permission / Delegate feature
    // (previously registered under the phantom name "Delegate").
    ("PermissionDelegationV1_1", false),
    // Implemented by the NFTokenModify transactor, which gates on it.
    ("DynamicNFT", true),
    // Registered so the on-ledger amendment id is recognised; rxrpl escrow is
    // still XRP-only, so this is not voted for by default.
    ("TokenEscrow", false),
    ("fixPayChanCancelAfter", false),
    ("fixAMMClawbackRounding", true),
    ("fixDirectoryLimit", true),
    ("fixIncludeKeyletFields", true),
    ("fixMPTDeliveredAmount", true),
    ("fixPriceOracleOrder", true),
    ("fixTokenEscrowV1", true),
    ("LendingProtocol", false),
    ("PermissionedDEX", false),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::feature_id;

    #[test]
    fn empty_registry() {
        let reg = FeatureRegistry::new();
        assert!(reg.is_empty());
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = FeatureRegistry::new();
        let f = Feature::new("TestFeature", true);
        let id = reg.register(f);

        assert!(reg.get(&id).is_some());
        assert_eq!(reg.get(&id).unwrap().name, "TestFeature");
        assert!(reg.get_by_name("TestFeature").is_some());
    }

    #[test]
    fn known_amendments_registry() {
        let reg = FeatureRegistry::with_known_amendments();
        assert!(!reg.is_empty());

        // MultiSign should be retired
        let ms = reg.get_by_name("MultiSign").unwrap();
        assert!(ms.retired);
        assert_eq!(ms.id, feature_id("MultiSign"));

        // FlowCross should be supported with default yes
        let fc = reg.get_by_name("FlowCross").unwrap();
        assert!(!fc.retired);
        assert!(fc.default_vote);
    }

    #[test]
    fn id_for_name() {
        let reg = FeatureRegistry::with_known_amendments();
        let id = reg.id_for_name("MultiSignReserve").unwrap();
        assert_eq!(id, feature_id("MultiSignReserve"));
    }

    #[test]
    fn new_retired_amendments_present() {
        let reg = FeatureRegistry::with_known_amendments();
        for name in [
            "fixFrozenLPTokenTransfer",
            "fixEnforceNFTokenTrustlineV2",
            "fixInnerObjTemplate2",
            "fixInvalidTxFlags",
            "fixNFTokenPageLinks",
            "ExpandedSignerList",
        ] {
            let f = reg
                .get_by_name(name)
                .unwrap_or_else(|| panic!("{name} not found"));
            assert!(f.retired, "{name} should be retired");
        }
    }

    #[test]
    fn new_supported_amendments_present() {
        let reg = FeatureRegistry::with_known_amendments();
        for (name, expected_vote) in [
            ("fixAMMClawbackRounding", true),
            ("fixDirectoryLimit", true),
            ("fixIncludeKeyletFields", true),
            ("fixMPTDeliveredAmount", true),
            ("fixPriceOracleOrder", true),
            ("fixTokenEscrowV1", true),
        ] {
            let f = reg
                .get_by_name(name)
                .unwrap_or_else(|| panic!("{name} not found"));
            assert!(!f.retired, "{name} should not be retired");
            assert_eq!(
                f.default_vote, expected_vote,
                "{name} default_vote mismatch"
            );
        }
    }

    #[test]
    fn no_duplicate_fixammv1_3() {
        let reg = FeatureRegistry::with_known_amendments();
        // fixAMMv1_3 should exist exactly once as retired
        let f = reg.get_by_name("fixAMMv1_3").unwrap();
        assert!(f.retired);
    }

    #[test]
    fn amendment_blocked_detection_predicate() {
        use crate::rules::Rules;
        use std::collections::HashSet;

        let reg = FeatureRegistry::with_known_amendments();
        let known: HashSet<Hash256> = reg.known_ids().copied().collect();

        // A clearly-unknown amendment id is not in the registry.
        let unknown = Hash256::new([0xAB; 32]);
        assert!(reg.get(&unknown).is_none());

        // Rules that enable the unknown amendment trip the halt predicate.
        let mut enabled: Vec<Hash256> = known.iter().take(3).copied().collect();
        enabled.push(unknown);
        let rules_with_unknown = Rules::from_enabled(enabled);
        assert!(
            rules_with_unknown.iter().any(|id| !known.contains(id)),
            "rules enabling an unknown amendment must be flagged as blocked"
        );

        // Rules whose enabled set is a subset of known ids do NOT trip it.
        let rules_all_known = Rules::from_enabled(known.iter().copied().collect::<Vec<_>>());
        assert!(
            !rules_all_known.iter().any(|id| !known.contains(id)),
            "rules with only known amendments must not be flagged"
        );
    }
}
