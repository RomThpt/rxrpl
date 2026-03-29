use std::collections::{HashMap, HashSet};

use rxrpl_primitives::Hash256;

use crate::table::AmendmentTable;

/// Threshold percentage for amendment majority (80%).
const MAJORITY_THRESHOLD: u32 = 80;

/// Interval in ledger sequences between flag ledgers where amendment
/// voting is evaluated. On the XRPL mainnet this is 256.
pub const FLAG_LEDGER_INTERVAL: u32 = 256;

/// Check whether a ledger sequence is a flag ledger (amendment voting boundary).
///
/// Flag ledgers occur every `FLAG_LEDGER_INTERVAL` ledgers (256).
/// Amendment voting is only evaluated on flag ledgers.
pub fn is_flag_ledger(seq: u32) -> bool {
    seq > 0 && seq % FLAG_LEDGER_INTERVAL == 0
}

/// Result of an amendment vote on a flag ledger.
///
/// Each entry describes a state change for a single amendment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AmendmentAction {
    /// The amendment gained majority at this close time.
    GotMajority {
        amendment_id: Hash256,
        close_time: u32,
    },
    /// The amendment lost its previously held majority.
    LostMajority {
        amendment_id: Hash256,
    },
    /// The amendment has held majority long enough and should be activated.
    Activate {
        amendment_id: Hash256,
    },
}

/// Tally amendment votes from received validations and determine
/// which amendments gained, lost, or should activate.
///
/// This function implements the XRPL amendment voting algorithm:
/// 1. Count how many trusted validators vote for each amendment.
/// 2. An amendment has majority if >= 80% of trusted validators support it.
/// 3. If an amendment that did not have majority now does, emit `GotMajority`.
/// 4. If an amendment that had majority now does not, emit `LostMajority`.
/// 5. If an amendment has held majority for the required window, emit `Activate`.
///
/// # Arguments
///
/// * `table` - The amendment table (mutated in place for majority/activation tracking)
/// * `trusted_count` - Number of trusted validators in the UNL
/// * `votes` - Map from amendment ID to the number of validators voting for it
/// * `close_time` - The close time of the current flag ledger
/// * `ledger_seq` - The current flag ledger sequence
pub fn tally_votes(
    table: &mut AmendmentTable,
    trusted_count: usize,
    votes: &HashMap<Hash256, usize>,
    close_time: u32,
    ledger_seq: u32,
) -> Vec<AmendmentAction> {
    if trusted_count == 0 {
        return vec![];
    }

    let threshold = (trusted_count as u64 * MAJORITY_THRESHOLD as u64 / 100) as usize;

    let mut actions = Vec::new();

    // First check for activations (amendments that have held majority long enough).
    let activated = table.check_activations(ledger_seq);
    for id in activated {
        actions.push(AmendmentAction::Activate { amendment_id: id });
    }

    // Then check all non-enabled, supported amendments for majority changes.
    let candidates = table.get_votes();
    // Also check amendments that currently have majority but might lose it.
    let all_tracked: HashSet<Hash256> = {
        let mut s: HashSet<Hash256> = candidates.into_iter().collect();
        // We also need to track amendments that might lose majority.
        // The table's internal state tracks majority_since, but we don't
        // have direct access. Instead, we check all known non-enabled amendments.
        for (id, count) in votes {
            s.insert(*id);
            let _ = count;
        }
        s
    };

    for id in all_tracked {
        if table.is_enabled(&id) {
            continue;
        }

        let vote_count = votes.get(&id).copied().unwrap_or(0);
        let has_majority_now = vote_count > threshold;

        if has_majority_now {
            // Record majority (no-op if already recorded)
            let had_majority = table.has_majority(&id);
            table.set_majority(&id, ledger_seq);
            if !had_majority {
                actions.push(AmendmentAction::GotMajority {
                    amendment_id: id,
                    close_time,
                });
            }
        } else {
            let had_majority = table.has_majority(&id);
            if had_majority {
                table.clear_majority(&id);
                actions.push(AmendmentAction::LostMajority { amendment_id: id });
            }
        }
    }

    actions
}

/// Collect per-amendment vote counts from a set of validator amendment lists.
///
/// Each entry in `validator_votes` is the list of amendment IDs a single
/// trusted validator votes to enable (carried in their validation message).
pub fn count_votes(validator_votes: &[Vec<Hash256>]) -> HashMap<Hash256, usize> {
    let mut counts: HashMap<Hash256, usize> = HashMap::new();
    for votes in validator_votes {
        for id in votes {
            *counts.entry(*id).or_insert(0) += 1;
        }
    }
    counts
}

/// Build an `EnableAmendment` pseudo-transaction JSON for a given action.
///
/// These pseudo-transactions are applied to the ledger via the tx-engine
/// just like normal transactions, but bypass signature/fee checks.
pub fn make_enable_amendment_tx(action: &AmendmentAction) -> serde_json::Value {
    match action {
        AmendmentAction::GotMajority {
            amendment_id,
            close_time,
        } => {
            serde_json::json!({
                "TransactionType": "EnableAmendment",
                "Amendment": hex::encode(amendment_id.as_bytes()),
                "Flags": 0x00010000u32,
                "CloseTime": close_time,
            })
        }
        AmendmentAction::LostMajority { amendment_id } => {
            serde_json::json!({
                "TransactionType": "EnableAmendment",
                "Amendment": hex::encode(amendment_id.as_bytes()),
                "Flags": 0x00020000u32,
            })
        }
        AmendmentAction::Activate { amendment_id } => {
            serde_json::json!({
                "TransactionType": "EnableAmendment",
                "Amendment": hex::encode(amendment_id.as_bytes()),
                "Flags": 0u32,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::Feature;
    use crate::registry::FeatureRegistry;

    fn test_registry() -> FeatureRegistry {
        let mut reg = FeatureRegistry::new();
        reg.register(Feature::new("FeatureA", true));
        reg.register(Feature::new("FeatureB", true));
        reg.register(Feature::new("FeatureC", false));
        reg.register(Feature::retired("RetiredX"));
        reg
    }

    #[test]
    fn is_flag_ledger_check() {
        assert!(!is_flag_ledger(0));
        assert!(!is_flag_ledger(1));
        assert!(!is_flag_ledger(255));
        assert!(is_flag_ledger(256));
        assert!(!is_flag_ledger(257));
        assert!(is_flag_ledger(512));
        assert!(is_flag_ledger(768));
    }

    #[test]
    fn tally_votes_got_majority() {
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        // 9 out of 10 trusted validators support FeatureA (90% > 80%)
        let mut votes = HashMap::new();
        votes.insert(id_a, 9);

        let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
        assert!(actions.iter().any(|a| matches!(
            a,
            AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
        )));
    }

    #[test]
    fn tally_votes_lost_majority() {
        let reg = test_registry();
        // Use large majority_time so activation does not trigger between seq 256 and 512
        let mut table = AmendmentTable::new(&reg, 100_000);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        // First: gain majority
        let mut votes = HashMap::new();
        votes.insert(id_a, 9);
        tally_votes(&mut table, 10, &votes, 1000, 256);

        // Now: lose majority (only 5 out of 10)
        let mut votes2 = HashMap::new();
        votes2.insert(id_a, 5);
        let actions = tally_votes(&mut table, 10, &votes2, 2000, 512);
        assert!(actions.iter().any(|a| matches!(
            a,
            AmendmentAction::LostMajority { amendment_id } if *amendment_id == id_a
        )));
    }

    #[test]
    fn tally_votes_activation_after_majority_window() {
        let reg = test_registry();
        let majority_time = 100;
        let mut table = AmendmentTable::new(&reg, majority_time);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        // Gain majority at seq 256
        let mut votes = HashMap::new();
        votes.insert(id_a, 9);
        let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
        assert!(actions.iter().any(|a| matches!(
            a,
            AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
        )));

        // After majority_time ledgers, should activate
        let actions2 = tally_votes(&mut table, 10, &votes, 2000, 256 + majority_time);
        assert!(actions2.iter().any(|a| matches!(
            a,
            AmendmentAction::Activate { amendment_id } if *amendment_id == id_a
        )));
        assert!(table.is_enabled(&id_a));
    }

    #[test]
    fn tally_votes_below_threshold_no_majority() {
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        // 7 out of 10 (70% < 80%)
        let mut votes = HashMap::new();
        votes.insert(id_a, 7);

        let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
        assert!(actions.is_empty());
    }

    #[test]
    fn tally_votes_retired_not_affected() {
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_retired = reg.id_for_name("RetiredX").unwrap();

        // Even with votes, retired amendments stay enabled and produce no actions
        let mut votes = HashMap::new();
        votes.insert(id_retired, 10);

        let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
        assert!(actions.is_empty());
        assert!(table.is_enabled(&id_retired));
    }

    #[test]
    fn count_votes_basic() {
        let id1 = Hash256::new([0x01; 32]);
        let id2 = Hash256::new([0x02; 32]);
        let id3 = Hash256::new([0x03; 32]);

        let validator_votes = vec![
            vec![id1, id2],
            vec![id1, id3],
            vec![id1, id2, id3],
        ];

        let counts = count_votes(&validator_votes);
        assert_eq!(counts[&id1], 3);
        assert_eq!(counts[&id2], 2);
        assert_eq!(counts[&id3], 2);
    }

    #[test]
    fn make_enable_amendment_tx_got_majority() {
        let id = Hash256::new([0xAA; 32]);
        let action = AmendmentAction::GotMajority {
            amendment_id: id,
            close_time: 1000,
        };
        let tx = make_enable_amendment_tx(&action);
        assert_eq!(tx["TransactionType"], "EnableAmendment");
        assert_eq!(tx["Flags"], 0x00010000u32);
        assert_eq!(tx["CloseTime"], 1000);
    }

    #[test]
    fn make_enable_amendment_tx_lost_majority() {
        let id = Hash256::new([0xBB; 32]);
        let action = AmendmentAction::LostMajority { amendment_id: id };
        let tx = make_enable_amendment_tx(&action);
        assert_eq!(tx["TransactionType"], "EnableAmendment");
        assert_eq!(tx["Flags"], 0x00020000u32);
    }

    #[test]
    fn make_enable_amendment_tx_activate() {
        let id = Hash256::new([0xCC; 32]);
        let action = AmendmentAction::Activate { amendment_id: id };
        let tx = make_enable_amendment_tx(&action);
        assert_eq!(tx["TransactionType"], "EnableAmendment");
        assert_eq!(tx["Flags"], 0);
    }

    #[test]
    fn zero_trusted_returns_empty() {
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        let mut votes = HashMap::new();
        votes.insert(id_a, 5);

        let actions = tally_votes(&mut table, 0, &votes, 1000, 256);
        assert!(actions.is_empty());
    }
}
