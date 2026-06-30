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

/// Amendment hash IDs pre-activated in rippled-2.6.2's genesis ledger #1.
///
/// Captured from `rippled --standalone` (private network_id 10000) by reading
/// the `Amendments` SLE at ledger 1: `ledger_data` JSON-RPC, filtered to
/// `LedgerEntryType=Amendments`. These 28 ids are the rippled-2.6.2 set of
/// "always-on at genesis" amendments — distinct from the (larger) list of
/// retired amendments tracked at runtime.
///
/// Used by `Node::genesis_with_funded_account_and_store` to seed the
/// `Amendments` SLE so that rxrpl's genesis hash matches rippled-2.6.2's
/// (cross-impl convergence at seq=1, issue #76).
///
/// Hex strings are uppercase to match the wire-format hex used in
/// EnableAmendment pseudo-tx payloads.
pub const GENESIS_AMENDMENTS_HEX: &[&str] = &[
    "00C1FC4A53E60AB02C864641002B3172F38677E29C26C5406685179B37E1EDAC",
    "12523DF04B553A0B1AD74F42DDB741DE8DC06A03FC089A0EF197E2A87F1D8107",
    "157D2D480E006395B76F948E3E07A45A05FE10230D88A7993C71F97AE4B1F2D1",
    "1F4AFA8FA1BC8827AD4C0F682C03A8B671DCDF6B5C4DE36D44243A684103EF88",
    "25BA44241B3BD880770BFA4DA21C7180576831855368CBEC6A3154FDE4A7676E",
    "2CD5286D8D687E98B41102BDD797198E81EA41DF7BD104E6561FEB104EFF2561",
    "30CD365592B8EE40489BA01AE2F7555CAC9C983145871DC82A42A31CF5BAE7D9",
    "3CBC5C4E630A1B82380295CDA84B32B49DD066602E74E39B85EF64137FA65194",
    "452F5906C46D46F407883344BFDD90E672B672C5E9943DB4891E3A34FEEEB9DB",
    "4F46DF03559967AC60F2EB272FEFE3928A7594A45FF774B87A7E540DB0F8F068",
    "586480873651E106F1D6339B0C4A8945BA705A777F3F4524626FF1FC07EFE41D",
    "58BE9B5968C4DA7C59BA900961828B113E5490699B21877DEF9A31E9D0FE5D5F",
    "5D08145F0A4983F23AFFFF514E83FAD355C5ABFBB6CAB76FB5BC8519FF5F33BE",
    "621A0B264970359869E3C0363A899909AAB7A887C8B73519E4ECF952D33258A8",
    "67A34F2CF55BFC0F93AACD5B281413176FEE195269FA6D95219A2DF738671172",
    "7117E2EC2DBF119CA55181D69819F1999ECEE1A0225A7FD2B9ED47940968479C",
    "740352F2412A9909880C23A559FCECEDA3BE2126FED62FC7660D628A06927F11",
    "89308AF3B8B10B7192C4E613E1D2E4D9BA64B2EE2D5232402AE82A6A7220D953",
    "8F81B066ED20DAECA20DF57187767685EEF3980B228E0667A650BAF24426D3B4",
    "955DF3FA5891195A9DAEFA1DDC6BB244B545DDE1BAA84CBB25D5F12A8DA68A0C",
    "AF8DF7465C338AE64B1E937D6C8DA138C0D63AD5134A68792BBBE1F63356C422",
    "B4E4F5D2D6FB84DF7399960A732309C9FD530EAE5941838160042833625A6076",
    "B6B3EEDC0267AB50491FDC450A398AF30DBCD977CECED8BEF2499CAB5DAC19E2",
    "C4483A1896170C66C098DEA5B0E024309C60DC960DE5F01CD7AF986AA3D9AD37",
    "CA7C02118BA27599528543DFE77BA6838D1B0F43B447D4D7F53523CE6A0E9AC2",
    "DF8B4536989BDACE3F934F29423848B9F1D76D09BE6A1FCFE7E7F06AA26ABEAD",
    "F64E1EABBE79D55B3BB82020516CEC2C582A98A6BFE20FBE9BB6A0D233418064",
    "FBD513F1B893AC765B78F250E6FFA6A11B573209D1842ADC787C850696741288",
];

pub const PRESET: &[(&str, bool)] = &[
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
    ("PermissionDelegationV1_1", false),
    ("DynamicNFT", true),
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
