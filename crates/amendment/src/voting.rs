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
    LostMajority { amendment_id: Hash256 },
    /// The amendment has held majority long enough and should be activated.
    Activate { amendment_id: Hash256 },
}

/// Tally amendment votes from received validations and determine
/// which amendments gained, lost, or should activate.
///
/// This function implements the XRPL amendment voting algorithm:
/// 1. Count how many trusted validators vote for each amendment.
/// 2. An amendment has majority when its support exceeds
///    `max(1, trustedCount * 80 / 100)` (rippled `AmendmentSet`: strict `>`,
///    with a single trusted validator the one exception that uses `>=`).
/// 3. If an amendment that did not have majority now does, emit `GotMajority`.
/// 4. If an amendment that had majority now does not, emit `LostMajority`.
/// 5. If an amendment has held majority for the required window, emit `Activate`.
///
/// # Arguments
///
/// * `table` - The amendment table (mutated in place for majority/activation tracking)
/// * `trusted_count` - Number of trusted validators in the UNL
/// * `votes` - Map from amendment ID to the number of validators voting for it
/// * `close_time` - The close time (seconds) of the current flag ledger; this
///   is the time reference for both majority recording and the activation
///   window (rippled measures the two-week window in close-time seconds).
/// * `_ledger_seq` - The current flag ledger sequence. Retained for call-site
///   symmetry with the consensus loop; the activation window is now measured in
///   close-time seconds, so the sequence is no longer used inside the tally.
pub fn tally_votes(
    table: &mut AmendmentTable,
    trusted_count: usize,
    votes: &HashMap<Hash256, usize>,
    close_time: u32,
    _ledger_seq: u32,
) -> Vec<AmendmentAction> {
    if trusted_count == 0 {
        return vec![];
    }

    // rippled `AmendmentSet` (AmendmentTable.cpp): the threshold is
    // `max(1, trustedValidations * 80 / 100)` and `passes()` compares with a
    // strict `>` EXCEPT when there is exactly one trusted validator, which is
    // the one case that uses `>=` (otherwise majority would be unreachable).
    // Mirror it exactly — the previous code dropped the `max(1, ..)` floor and
    // the single-validator `>=` exception, so this keeps amendment activation
    // byte-faithful with rippled 3.2 and avoids a cross-impl divergence.
    let threshold = std::cmp::max(
        1,
        (trusted_count as u64 * MAJORITY_THRESHOLD as u64 / 100) as usize,
    );

    let mut actions = Vec::new();

    // First check for activations (amendments that have held majority long
    // enough). The activation window is measured in close-time seconds, so we
    // pass `close_time` here (not the ledger sequence).
    let activated = table.check_activations(close_time);
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

    // Iterate in deterministic id order: HashSet iteration is unordered, so
    // emitting GotMajority/LostMajority actions in id-sorted order keeps the
    // resulting pseudo-transaction sequence (and ledger hash) identical across
    // implementations.
    let mut all_tracked: Vec<Hash256> = all_tracked.into_iter().collect();
    all_tracked.sort();

    for id in all_tracked {
        if table.is_enabled(&id) {
            continue;
        }

        let vote_count = votes.get(&id).copied().unwrap_or(0);
        let has_majority_now = if trusted_count == 1 {
            vote_count >= threshold
        } else {
            vote_count > threshold
        };

        if has_majority_now {
            // Record majority (no-op if already recorded). Majority is anchored
            // to the close time, which drives the close-time activation window.
            let had_majority = table.has_majority(&id);
            table.set_majority(&id, close_time);
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
        // majority_time is now in close-time SECONDS, not ledgers.
        let majority_time: u32 = 100;
        let mut table = AmendmentTable::new(&reg, majority_time);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        // Gain majority at close_time = 1000.
        let mut votes = HashMap::new();
        votes.insert(id_a, 9);
        let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
        assert!(actions.iter().any(|a| matches!(
            a,
            AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
        )));

        // Exactly `majority_time` SECONDS of close-time later, it activates.
        let actions2 = tally_votes(&mut table, 10, &votes, 1000 + majority_time, 512);
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
    fn tally_votes_threshold_boundary_matches_rippled() {
        // rippled `AmendmentSet`: threshold = max(1, N*80/100), strict `>` for
        // N > 1. For N = 10, threshold = 8 and support must be STRICTLY greater
        // than 8 (i.e. >= 9). Exactly 80% (8/10) is therefore NOT majority —
        // this asserts we did not introduce the lenient `>= 80%` off-by-one
        // that would diverge from rippled at round UNL sizes.
        let reg = test_registry();

        // 8 of 10 (exactly 80%) -> NO majority (rippled requires > 8).
        {
            let mut table = AmendmentTable::new(&reg, 100);
            let id_a = reg.id_for_name("FeatureA").unwrap();
            let mut votes = HashMap::new();
            votes.insert(id_a, 8);
            let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
            assert!(
                actions.is_empty(),
                "8/10 (exactly 80%) must not reach majority under rippled's strict >"
            );
        }

        // 9 of 10 (90%) -> majority.
        {
            let mut table = AmendmentTable::new(&reg, 100);
            let id_a = reg.id_for_name("FeatureA").unwrap();
            let mut votes = HashMap::new();
            votes.insert(id_a, 9);
            let actions = tally_votes(&mut table, 10, &votes, 1000, 256);
            assert!(actions.iter().any(|a| matches!(
                a,
                AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
            )));
        }
    }

    #[test]
    fn tally_votes_threshold_lock_in_across_unl_sizes() {
        // Lock the rippled threshold formula so a future edit cannot silently
        // weaken it. For each trusted-validator count N > 1, the threshold is
        // `max(1, floor(N * 80 / 100))` with a strict `>`, so:
        //   * exactly `floor(0.8 * N)` yes-votes is NOT majority, and
        //   * `floor(0.8 * N) + 1` yes-votes IS majority.
        let reg = test_registry();
        for n in [2usize, 3, 5, 8, 10, 20, 28, 100] {
            let floor80 = n * 80 / 100;

            // floor(0.8 * N) votes -> below threshold, no majority.
            {
                let mut table = AmendmentTable::new(&reg, 100);
                let id_a = reg.id_for_name("FeatureA").unwrap();
                let mut votes = HashMap::new();
                votes.insert(id_a, floor80);
                let actions = tally_votes(&mut table, n, &votes, 1000, 256);
                assert!(
                    !actions.iter().any(|a| matches!(
                        a,
                        AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
                    )),
                    "N={n}: {floor80} votes (exactly floor(0.8N)) must NOT reach majority"
                );
            }

            // floor(0.8 * N) + 1 votes -> above threshold, majority.
            {
                let mut table = AmendmentTable::new(&reg, 100);
                let id_a = reg.id_for_name("FeatureA").unwrap();
                let mut votes = HashMap::new();
                votes.insert(id_a, floor80 + 1);
                let actions = tally_votes(&mut table, n, &votes, 1000, 256);
                assert!(
                    actions.iter().any(|a| matches!(
                        a,
                        AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
                    )),
                    "N={n}: {} votes (floor(0.8N)+1) must reach majority",
                    floor80 + 1
                );
            }
        }
    }

    #[test]
    fn tally_votes_activation_requires_full_close_time_window() {
        // The 2-week window is measured in close-time SECONDS: an amendment must
        // hold majority for at least `majority_time` seconds before it activates.
        let reg = test_registry();
        let majority_time: u32 = 1000;
        let mut table = AmendmentTable::new(&reg, majority_time);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        let mut votes = HashMap::new();
        votes.insert(id_a, 9);

        // Gain majority at close_time = 5000.
        tally_votes(&mut table, 10, &votes, 5000, 256);

        // One second short of the window -> still NOT activated.
        let almost = tally_votes(&mut table, 10, &votes, 5000 + majority_time - 1, 512);
        assert!(
            !almost
                .iter()
                .any(|a| matches!(a, AmendmentAction::Activate { .. })),
            "activation must not fire before the full close-time window elapses"
        );
        assert!(!table.is_enabled(&id_a));

        // Exactly at the window -> activates.
        let at = tally_votes(&mut table, 10, &votes, 5000 + majority_time, 768);
        assert!(
            at.iter().any(|a| matches!(
                a,
                AmendmentAction::Activate { amendment_id } if *amendment_id == id_a
            )),
            "activation must fire once the full close-time window has elapsed"
        );
        assert!(table.is_enabled(&id_a));
    }

    #[test]
    fn tally_votes_single_validator_uses_ge() {
        // rippled `AmendmentSet::passes`: with exactly one trusted validator the
        // comparison is `>=` (threshold floored at 1), otherwise a solo UNL
        // could never enact an amendment. One vote out of one -> majority.
        let reg = test_registry();
        let mut table = AmendmentTable::new(&reg, 100);
        let id_a = reg.id_for_name("FeatureA").unwrap();

        let mut votes = HashMap::new();
        votes.insert(id_a, 1);

        let actions = tally_votes(&mut table, 1, &votes, 1000, 256);
        assert!(actions.iter().any(|a| matches!(
            a,
            AmendmentAction::GotMajority { amendment_id, .. } if *amendment_id == id_a
        )));
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

        let validator_votes = vec![vec![id1, id2], vec![id1, id3], vec![id1, id2, id3]];

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
