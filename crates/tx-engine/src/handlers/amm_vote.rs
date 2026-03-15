use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// Maximum number of vote slots in an AMM.
const MAX_VOTE_SLOTS: usize = 8;

pub struct AMMVoteTransactor;

impl Transactor for AMMVoteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx.tx.get("Asset2").ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        let trading_fee = helpers::get_u32_field(ctx.tx, "TradingFee")
            .ok_or(TransactionResult::TemMalformed)?;
        if trading_fee > 1000 {
            return Err(TransactionResult::TemBadFee);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        amm_helpers::read_amm(ctx.view, &amm_key)?;

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let new_fee = helpers::get_u32_field(ctx.tx, "TradingFee")
            .ok_or(TransactionResult::TemMalformed)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // Update VoteSlots
        let vote_entry = serde_json::json!({
            "Account": account_str,
            "TradingFee": new_fee,
        });

        let vote_slots = amm
            .get_mut("VoteSlots")
            .and_then(|v| v.as_array_mut());

        match vote_slots {
            Some(slots) => {
                // Replace existing vote by same account, or add new
                let mut found = false;
                for slot in slots.iter_mut() {
                    if slot.get("Account").and_then(|v| v.as_str()) == Some(account_str) {
                        *slot = vote_entry.clone();
                        found = true;
                        break;
                    }
                }
                if !found {
                    if slots.len() >= MAX_VOTE_SLOTS {
                        // Replace the first slot (oldest)
                        slots[0] = vote_entry;
                    } else {
                        slots.push(vote_entry);
                    }
                }
            }
            None => {
                amm["VoteSlots"] = serde_json::json!([vote_entry]);
            }
        }

        // Compute new TradingFee as average of all vote slots
        let slots = amm["VoteSlots"].as_array().cloned().unwrap_or_default();
        if !slots.is_empty() {
            let total: u64 = slots
                .iter()
                .filter_map(|s| s.get("TradingFee").and_then(|v| v.as_u64()))
                .sum();
            let avg = (total / slots.len() as u64) as u32;
            amm["TradingFee"] = serde_json::Value::from(avg);
        }

        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment voter's sequence
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let acct_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_amm() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ALICE, 100_000_000u64), (BOB, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let tx_ref = serde_json::json!({
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
        });
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx_ref).unwrap();
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Creator": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "PoolBalance1": "10000000",
            "PoolBalance2": "5000000",
            "LPTokenBalance": "5000000",
            "TradingFee": 500,
            "VoteSlots": [],
            "AuctionSlot": null,
            "Flags": 0,
        });
        ledger
            .put_state(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn vote_sets_trading_fee() {
        let ledger = setup_with_amm();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 300,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMVoteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        // Single vote, so fee is just the vote value
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 300);

        let slots = amm["VoteSlots"].as_array().unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["Account"].as_str().unwrap(), BOB);
        assert_eq!(slots[0]["TradingFee"].as_u64().unwrap(), 300);
    }

    #[test]
    fn multiple_votes_average() {
        let ledger = setup_with_amm();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // First vote by ALICE
        let tx1 = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 400,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx1,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();

        // Second vote by BOB
        let tx2 = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 600,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx2).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        // Average of 400 and 600 = 500
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 500);
        assert_eq!(amm["VoteSlots"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn vote_replaces_existing() {
        let ledger = setup_with_amm();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // First vote
        let tx1 = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 400,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx1,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();

        // Second vote by same account
        let tx2 = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 800,
            "Fee": "12",
            "Sequence": 2,
        });
        let mut ctx = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx2).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        // Should still be 1 slot, updated to 800
        assert_eq!(amm["VoteSlots"].as_array().unwrap().len(), 1);
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 800);
    }

    #[test]
    fn reject_fee_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 1001,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AMMVoteTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadFee)
        );
    }

    #[test]
    fn reject_missing_trading_fee() {
        let tx = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AMMVoteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn vote_increments_sequence() {
        let ledger = setup_with_amm();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 300,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        AMMVoteTransactor.apply(&mut ctx).unwrap();

        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_bytes = sandbox.read(&bob_key).unwrap();
        let bob: serde_json::Value = serde_json::from_slice(&bob_bytes).unwrap();
        assert_eq!(bob["Sequence"].as_u64().unwrap(), 2);
    }
}
