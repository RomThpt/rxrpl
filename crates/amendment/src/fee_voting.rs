//! Fee / reserve voting at flag ledgers.
//!
//! Faithful port of rippled's `FeeVoteImpl` (`src/xrpld/app/misc/FeeVoteImpl.cpp`).
//!
//! At each flag ledger the network agrees on the base fee, base reserve and
//! owner-reserve increment. Every trusted validator advertises the value it
//! *wants* for each parameter in its validation; at the ledger after the flag
//! ledger the votes are tallied and, if they move the current value, a `SetFee`
//! pseudo-transaction is injected. This module is the pure tally half (the
//! [`VotableValue`] weighted-median and the [`tally_fee_votes`] driver) plus the
//! [`make_set_fee_tx`] pseudo-transaction builder; collecting the votes from
//! validations and injecting the pseudo-tx lives in the node consensus loop
//! (mirrors amendment voting in [`crate::voting`]).
//!
//! Consensus-critical: the tally MUST match rippled bit-for-bit or a divergent
//! `FeeSettings` object forks `account_hash`. The algorithm below reproduces
//! `VotableValue::getVotes` exactly — ascending iteration, an inclusive
//! `[min(current,target), max(current,target)]` range, strict `> weight` (so a
//! tie resolves to the lowest key), and the constructor's self-vote for the
//! node's own target.

use std::collections::BTreeMap;

/// The three votable fee parameters, as raw integers exactly as they appear in
/// the `FeeSettings` ledger object and the validation vote fields.
///
/// Post-`XRPFees` all three are drops; pre-`XRPFees` `base_fee` is drops while
/// `reserve_base`/`reserve_increment` are legacy fee units. The tally is
/// unit-agnostic — the caller supplies matching current/target/vote values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeSettingsVote {
    pub base_fee: u64,
    pub reserve_base: u64,
    pub reserve_increment: u64,
}

/// One trusted validator's fee vote, extracted from its validation.
///
/// `None` for a field means the validator did not advertise that parameter;
/// rippled treats a missing (or invalid) field as a vote for the *current*
/// value (`VotableValue::noVote`), so it still counts toward the weight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeeVoteEntry {
    pub base_fee: Option<u64>,
    pub reserve_base: Option<u64>,
    pub reserve_increment: Option<u64>,
}

/// Port of rippled's `detail::VotableValue`.
///
/// Tracks votes for a single fee parameter and selects the most-voted value
/// within `[min(current, target), max(current, target)]`.
#[derive(Debug)]
pub struct VotableValue {
    current: u64,
    target: u64,
    votes: BTreeMap<u64, u32>,
}

impl VotableValue {
    /// `current` is the flag ledger's value; `target` is this node's preferred
    /// value. The node's own vote for `target` is added immediately, exactly as
    /// rippled's constructor does (`++voteMap_[target_]`).
    pub fn new(current: u64, target: u64) -> Self {
        let mut votes = BTreeMap::new();
        votes.insert(target, 1);
        Self {
            current,
            target,
            votes,
        }
    }

    /// Record a trusted validator's vote for `vote`.
    pub fn add_vote(&mut self, vote: u64) {
        *self.votes.entry(vote).or_insert(0) += 1;
    }

    /// A missing or invalid vote field counts as a vote for the current value
    /// (rippled `noVote()` = `addVote(current_)`).
    pub fn no_vote(&mut self) {
        let c = self.current;
        self.add_vote(c);
    }

    /// rippled `getVotes()`: returns `(chosen_value, changed)`.
    ///
    /// Iterates votes in ascending key order and keeps the value with the
    /// strictly-highest count that lies within `[min(current,target),
    /// max(current,target)]`. The strict `>` plus ascending order means a tie
    /// resolves to the lowest key. `changed` is `chosen != current`.
    pub fn get_votes(&self) -> (u64, bool) {
        let lo = self.current.min(self.target);
        let hi = self.current.max(self.target);
        let mut our_vote = self.current;
        let mut weight: u32 = 0;
        for (&key, &val) in &self.votes {
            if key >= lo && key <= hi && val > weight {
                our_vote = key;
                weight = val;
            }
        }
        (our_vote, our_vote != self.current)
    }
}

/// Tally trusted validators' fee votes into new fee settings.
///
/// Mirrors `FeeVoteImpl::doVoting`. `current` is the flag ledger's `FeeSettings`;
/// `target` is this node's configured preference; `votes` is one [`FeeVoteEntry`]
/// per trusted validation (a `None` field → vote for current). Returns
/// `Some(new_settings)` iff at least one of the three parameters changed —
/// rippled only injects a `SetFee` in that case; `None` means no pseudo-tx.
pub fn tally_fee_votes(
    current: FeeSettingsVote,
    target: FeeSettingsVote,
    votes: &[FeeVoteEntry],
) -> Option<FeeSettingsVote> {
    let mut base = VotableValue::new(current.base_fee, target.base_fee);
    let mut reserve_base = VotableValue::new(current.reserve_base, target.reserve_base);
    let mut reserve_inc = VotableValue::new(current.reserve_increment, target.reserve_increment);

    for v in votes {
        match v.base_fee {
            Some(x) => base.add_vote(x),
            None => base.no_vote(),
        }
        match v.reserve_base {
            Some(x) => reserve_base.add_vote(x),
            None => reserve_base.no_vote(),
        }
        match v.reserve_increment {
            Some(x) => reserve_inc.add_vote(x),
            None => reserve_inc.no_vote(),
        }
    }

    let (base_fee, base_changed) = base.get_votes();
    let (reserve_base, rbase_changed) = reserve_base.get_votes();
    let (reserve_increment, rinc_changed) = reserve_inc.get_votes();

    if base_changed || rbase_changed || rinc_changed {
        Some(FeeSettingsVote {
            base_fee,
            reserve_base,
            reserve_increment,
        })
    } else {
        None
    }
}

/// Build the `SetFee` pseudo-transaction JSON for the voted fee settings.
///
/// Applied to the flag ledger's successor by the tx-engine, like the
/// `EnableAmendment` pseudo-tx ([`crate::voting::make_enable_amendment_tx`]).
/// `xrp_fees` selects the field variant per the `XRPFees` amendment: the
/// `*Drops` fields when enabled, the legacy fields (plus `ReferenceFeeUnits`)
/// otherwise. `LedgerSequence` is the flag ledger's successor
/// (`flag_ledger_seq + 1`), matching rippled's `sfLedgerSequence = lcl->seq() + 1`.
///
/// The canonical pseudo-tx skeleton fields — `Account = ACCOUNT_ZERO`,
/// `Sequence = 0`, `Fee = "0"`, `SigningPubKey = ""` — are included so the
/// serialized transaction (and therefore its id / the tx-tree hash) is
/// byte-identical to rippled's `FeeVoteImpl::doVoting` `STTx`. Verified against
/// a real mainnet SetFee (txid recomputes exactly). The tx-engine ignores them
/// on the pseudo-tx apply path but they are consensus-critical for the hash.
pub fn make_set_fee_tx(
    new: FeeSettingsVote,
    flag_ledger_seq: u32,
    xrp_fees: bool,
) -> serde_json::Value {
    let ledger_sequence = flag_ledger_seq + 1;
    // The all-zero AccountID (`ACCOUNT_ZERO`), base58check-encoded.
    const ACCOUNT_ZERO: &str = "rrrrrrrrrrrrrrrrrrrrrhoLvTp";
    if xrp_fees {
        serde_json::json!({
            "TransactionType": "SetFee",
            "Account": ACCOUNT_ZERO,
            "Sequence": 0u32,
            "Fee": "0",
            "SigningPubKey": "",
            "LedgerSequence": ledger_sequence,
            "BaseFeeDrops": new.base_fee.to_string(),
            "ReserveBaseDrops": new.reserve_base.to_string(),
            "ReserveIncrementDrops": new.reserve_increment.to_string(),
        })
    } else {
        // Pre-XRPFees: BaseFee is a UInt64 (drops, encoded as a string like the
        // handler expects); ReserveBase/ReserveIncrement are UInt32 legacy
        // units; ReferenceFeeUnits carries rippled's kFeeUnitsDeprecated (10).
        serde_json::json!({
            "TransactionType": "SetFee",
            "Account": ACCOUNT_ZERO,
            "Sequence": 0u32,
            "Fee": "0",
            "SigningPubKey": "",
            "LedgerSequence": ledger_sequence,
            // sfBaseFee is a UInt64; rippled renders UInt64 fields as an
            // uppercase hex string in JSON, and the codec parses them as hex.
            "BaseFee": format!("{:X}", new.base_fee),
            "ReserveBase": new.reserve_base as u32,
            "ReserveIncrement": new.reserve_increment as u32,
            "ReferenceFeeUnits": 10u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- VotableValue: exact rippled getVotes semantics ----

    #[test]
    fn self_vote_only_moves_to_target() {
        // No peers: only our self-vote for target exists. rippled returns target
        // (in range, weight 1 > 0) with changed = (target != current).
        let v = VotableValue::new(100, 200);
        assert_eq!(v.get_votes(), (200, true));
    }

    #[test]
    fn no_change_when_current_equals_target() {
        let mut v = VotableValue::new(100, 100);
        v.add_vote(100);
        v.add_vote(100);
        assert_eq!(v.get_votes(), (100, false));
    }

    #[test]
    fn consensus_middle_value_wins() {
        // current=100, target=200; three peers vote 150 -> 150 has the highest
        // count and is within range.
        let mut v = VotableValue::new(100, 200);
        v.add_vote(150);
        v.add_vote(150);
        v.add_vote(150);
        assert_eq!(v.get_votes(), (150, true));
    }

    #[test]
    fn out_of_range_votes_are_ignored() {
        // Votes outside [min(current,target), max] never win, even in a majority.
        let mut v = VotableValue::new(100, 200);
        v.add_vote(500);
        v.add_vote(500);
        v.add_vote(9999);
        // Only in-range value is our self-vote target=200 (weight 1).
        assert_eq!(v.get_votes(), (200, true));
    }

    #[test]
    fn range_is_inclusive_of_current_and_target() {
        // current=200, target=100 (descending): range [100,200] inclusive.
        let mut v = VotableValue::new(200, 100);
        v.add_vote(200); // == current, in range
        v.add_vote(200);
        // current 200 has weight 2 (>self-vote 100 weight1) -> stays current -> not changed.
        assert_eq!(v.get_votes(), (200, false));
    }

    #[test]
    fn tie_resolves_to_lowest_key() {
        // current=100, target=104. Peers split 2 for 101 and 2 for 103; the
        // self-vote gives 104 weight 1. Strict `>` + ascending means 101 (the
        // lowest of the tied top values) wins.
        let mut v = VotableValue::new(100, 104);
        v.add_vote(101);
        v.add_vote(101);
        v.add_vote(103);
        v.add_vote(103);
        assert_eq!(v.get_votes(), (101, true));
    }

    #[test]
    fn current_kept_when_it_has_the_most_votes() {
        // Many peers vote to keep current -> current wins, changed=false even
        // though our self-vote wanted target.
        let mut v = VotableValue::new(100, 200);
        for _ in 0..5 {
            v.no_vote(); // votes for current=100
        }
        assert_eq!(v.get_votes(), (100, false));
    }

    // ---- tally_fee_votes: doVoting driver ----

    #[test]
    fn tally_returns_none_when_nothing_changes() {
        // target == current on all three, peers agree -> no SetFee.
        let cur = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let votes = vec![FeeVoteEntry::default(); 5]; // all noVote -> current
        assert_eq!(tally_fee_votes(cur, cur, &votes), None);
    }

    #[test]
    fn tally_moves_toward_target_on_self_vote() {
        // No peers: self-vote alone moves each param to target -> Some.
        let cur = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let tgt = FeeSettingsVote {
            base_fee: 15,
            reserve_base: 20_000_000,
            reserve_increment: 5_000_000,
        };
        assert_eq!(tally_fee_votes(cur, tgt, &[]), Some(tgt));
    }

    #[test]
    fn tally_partial_change_still_emits() {
        // Only base_fee changes; reserve params stay -> Some with mixed values.
        let cur = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let tgt = FeeSettingsVote {
            base_fee: 12,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        assert_eq!(
            tally_fee_votes(cur, tgt, &[]),
            Some(FeeSettingsVote {
                base_fee: 12,
                reserve_base: 10_000_000,
                reserve_increment: 2_000_000,
            })
        );
    }

    #[test]
    fn tally_peer_majority_beats_self_vote() {
        // current=10, our target=20, but 4 peers vote 12 -> consensus 12.
        let cur = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let tgt = FeeSettingsVote {
            base_fee: 20,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let peer = FeeVoteEntry {
            base_fee: Some(12),
            reserve_base: Some(10_000_000),
            reserve_increment: Some(2_000_000),
        };
        let votes = vec![peer; 4];
        let out = tally_fee_votes(cur, tgt, &votes).unwrap();
        assert_eq!(out.base_fee, 12);
    }

    #[test]
    fn tally_missing_field_counts_as_current() {
        // Peers present but with no base_fee field -> those count as votes for
        // current, overwhelming our lone self-vote for target.
        let cur = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let tgt = FeeSettingsVote {
            base_fee: 20,
            reserve_base: 10_000_000,
            reserve_increment: 2_000_000,
        };
        let peer = FeeVoteEntry {
            base_fee: None, // noVote -> current
            reserve_base: None,
            reserve_increment: None,
        };
        let votes = vec![peer; 3];
        // base_fee: current=10 has weight 3, target=20 weight 1 -> stays 10.
        assert_eq!(tally_fee_votes(cur, tgt, &votes), None);
    }

    // ---- make_set_fee_tx: field shape the SetFeeTransactor consumes ----

    #[test]
    fn set_fee_tx_drops_variant() {
        let new = FeeSettingsVote {
            base_fee: 15,
            reserve_base: 20_000_000,
            reserve_increment: 5_000_000,
        };
        let tx = make_set_fee_tx(new, 256, true);
        assert_eq!(tx["TransactionType"], "SetFee");
        assert_eq!(tx["LedgerSequence"], 257);
        assert_eq!(tx["BaseFeeDrops"], "15");
        assert_eq!(tx["ReserveBaseDrops"], "20000000");
        assert_eq!(tx["ReserveIncrementDrops"], "5000000");
        assert!(tx.get("BaseFee").is_none());
        // Canonical pseudo-tx skeleton (byte-exactness vs rippled).
        assert_eq!(tx["Account"], "rrrrrrrrrrrrrrrrrrrrrhoLvTp");
        assert_eq!(tx["Sequence"], 0);
        assert_eq!(tx["Fee"], "0");
        assert_eq!(tx["SigningPubKey"], "");
    }

    #[test]
    fn set_fee_tx_legacy_variant() {
        let new = FeeSettingsVote {
            base_fee: 10,
            reserve_base: 200,
            reserve_increment: 50,
        };
        let tx = make_set_fee_tx(new, 512, false);
        assert_eq!(tx["TransactionType"], "SetFee");
        assert_eq!(tx["LedgerSequence"], 513);
        // sfBaseFee is a UInt64: rendered as an uppercase hex string (10 -> "A").
        assert_eq!(tx["BaseFee"], "A");
        assert_eq!(tx["ReserveBase"], 200);
        assert_eq!(tx["ReserveIncrement"], 50);
        assert_eq!(tx["ReferenceFeeUnits"], 10);
        assert!(tx.get("BaseFeeDrops").is_none());
        assert_eq!(tx["Account"], "rrrrrrrrrrrrrrrrrrrrrhoLvTp");
        assert_eq!(tx["Sequence"], 0);
        assert_eq!(tx["Fee"], "0");
        assert_eq!(tx["SigningPubKey"], "");
    }
}
