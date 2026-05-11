//! Amendment vote table compatible with rippled 2.6.2.
//!
//! Source of truth: rippled-2.6.2 `src/ripple/protocol/Feature.h` and
//! `src/ripple/protocol/impl/Feature.cpp` (look for `registerFeature` with
//! `VoteBehavior::DefaultYes` vs `DefaultNo`).
//!
//! For every amendment in [`crate::registry::SUPPORTED_AMENDMENTS`] there must
//! be exactly one entry here, with the vote value rippled-2.6.2 ships with.
//! Names not yet known to rippled 2.6.2 (post-2.6.2 amendments) should be
//! vetoed (`false`).
//!
//! This table is consensus-critical: a mismatch at any flag ledger (#256,
//! #512, …) produces a divergent `account_hash`, which surfaces as
//! `wrong_prev_ledger_detected` on the rippled peer and aborts mixed-validator
//! convergence (see issue #76).
//!
//! To regenerate from a live rippled-2.6.2:
//!
//! ```text
//! rippled feature --json | jq -r '
//!   .result.features
//!   | to_entries[]
//!   | "(\"\(.value.name)\", \(if .value.vetoed then "false" else "true" end)),"
//! '
//! ```
//!
//! Then sort/dedupe and replace this list. Compare against
//! `SUPPORTED_AMENDMENTS` and fix any missing entry.

pub const PRESET: &[(&str, bool)] = &[
    ("FlowCross", true),
    ("Flow", true),
    ("OwnerPaysFee", false),
    ("fixNFTokenNegOffer", true),
    ("fixNFTokenDirV1", true),
    ("MPTokensV1", true),
    ("Credentials", true),
    ("AMMClawback", true),
    ("InvariantsV1_1", true),
    ("PermissionedDomains", true),
    ("DeepFreeze", false),
    ("TokenKeg", false),
    ("SingleAssetVault", false),
    ("Batch", false),
    ("Delegate", false),
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
    use crate::registry::FeatureRegistry;

    #[test]
    fn every_preset_entry_is_a_known_amendment() {
        let reg = FeatureRegistry::with_known_amendments();
        for (name, _) in PRESET {
            assert!(
                reg.id_for_name(name).is_some(),
                "preset references unknown amendment {name}"
            );
        }
    }

    #[test]
    fn preset_covers_every_supported_amendment() {
        // Defends against drift: if SUPPORTED_AMENDMENTS adds a new name,
        // the preset must be updated in the same commit.
        let reg = FeatureRegistry::with_known_amendments();
        for feature in reg.all() {
            if feature.retired {
                continue;
            }
            let in_preset = PRESET.iter().any(|(n, _)| *n == feature.name);
            assert!(
                in_preset,
                "amendment {} is in registry but missing from rippled-2.6.2 preset",
                feature.name
            );
        }
    }
}
