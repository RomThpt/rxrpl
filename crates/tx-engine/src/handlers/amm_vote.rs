use rxrpl_amount::number::Number;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::AccountId;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

const MAX_VOTE_SLOTS: usize = 8;
const VOTE_WEIGHT_SCALE_FACTOR: i64 = 100_000;
const AUCTION_SLOT_DISCOUNTED_FEE_FRACTION: i64 = 10;

pub struct AMMVoteTransactor;

impl Transactor for AMMVoteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        let trading_fee =
            helpers::get_u32_field(ctx.tx, "TradingFee").ok_or(TransactionResult::TemMalformed)?;
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

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let new_fee = helpers::get_u32_field(ctx.tx, "TradingFee")
            .ok_or(TransactionResult::TemMalformed)? as i64;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        let amm_account = decode_account_id(
            amm["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let lp_currency = lp_currency_bytes(&amm)?;

        // T = total outstanding LP tokens (LPTokenBalance), as a Number.
        let lpt_balance = Number::from_iou(&amm_helpers::parse_iou_value(
            amm["LPTokenBalance"]["value"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        ));

        // The voter's own LP holding (lpTokensNew) and trading fee.
        let lp_new = lp_holds(ctx.view, &account_id, &amm_account, &lp_currency);

        let existing = amm
            .get("VoteSlots")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut num = Number::ZERO;
        let mut den = Number::ZERO;
        let mut updated: Vec<Value> = Vec::new();
        let mut found_account = false;
        let mut min: Option<(Number, usize, AccountId, i64)> = None;

        for slot in &existing {
            let entry = slot.get("VoteEntry").unwrap_or(slot);
            let entry_acct_str = entry
                .get("Account")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TefInternal)?;
            let entry_acct =
                decode_account_id(entry_acct_str).map_err(|_| TransactionResult::TefInternal)?;

            let mut lp = lp_holds(ctx.view, &entry_acct, &amm_account, &lp_currency);
            if lp.is_zero() {
                // No longer a liquidity provider: drop the entry.
                continue;
            }
            let mut fee = entry
                .get("TradingFee")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if entry_acct == account_id {
                lp = lp_new;
                fee = new_fee;
                found_account = true;
            }

            num = num.add(&Number::from_int(fee).mul(&lp));
            den = den.add(&lp);

            let vote_weight = vote_weight(&lp, &lpt_balance);
            updated.push(make_vote_entry(entry_acct_str, fee, vote_weight));

            let pos = updated.len() - 1;
            let is_new_min = match &min {
                None => true,
                Some((min_tokens, _, min_acct, min_fee)) => {
                    lp_lt(&lp, min_tokens)
                        || (lp == *min_tokens
                            && (fee < *min_fee
                                || (fee == *min_fee
                                    && entry_acct.as_bytes() < min_acct.as_bytes())))
                }
            };
            if is_new_min {
                min = Some((lp, pos, entry_acct, fee));
            }
        }

        if !found_account {
            let new_entry =
                make_vote_entry(account_str, new_fee, vote_weight(&lp_new, &lpt_balance));
            if updated.len() < MAX_VOTE_SLOTS {
                num = num.add(&Number::from_int(new_fee).mul(&lp_new));
                den = den.add(&lp_new);
                updated.push(new_entry);
            } else if let Some((min_tokens, min_pos, _, min_fee)) = min {
                if lp_lt(&min_tokens, &lp_new) || (lp_new == min_tokens && new_fee > min_fee) {
                    // Evict the least-token entry, then add the new voter.
                    num = num.sub(&Number::from_int(min_fee).mul(&min_tokens));
                    den = den.sub(&min_tokens);
                    num = num.add(&Number::from_int(new_fee).mul(&lp_new));
                    den = den.add(&lp_new);
                    updated[min_pos] = new_entry;
                }
                // else: slots full and not enough tokens — refresh only.
            }
        }

        amm["VoteSlots"] = Value::Array(updated);

        let fee = if den.is_zero() {
            0
        } else {
            num.div(&den).to_i64()
        };
        if fee != 0 {
            amm["TradingFee"] = Value::from(fee);
            if let Some(auction) = amm.get_mut("AuctionSlot") {
                let dfee = fee / AUCTION_SLOT_DISCOUNTED_FEE_FRACTION;
                if dfee != 0 {
                    auction["DiscountedFee"] = Value::from(dfee);
                } else if let Some(obj) = auction.as_object_mut() {
                    obj.remove("DiscountedFee");
                }
            }
        } else {
            if let Some(obj) = amm.as_object_mut() {
                obj.remove("TradingFee");
            }
            if let Some(auction) = amm.get_mut("AuctionSlot").and_then(|v| v.as_object_mut()) {
                auction.remove("DiscountedFee");
            }
        }

        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let account: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// `ammLPHolds`: the account's LP-token trust-line balance against the AMM
/// pseudo-account (issuer), in the holder's own perspective.
fn lp_holds(
    view: &dyn crate::view::read_view::ReadView,
    account: &AccountId,
    amm_account: &AccountId,
    lp_currency: &[u8; 20],
) -> Number {
    amm_helpers::iou_holding_number(view, account, amm_account, lp_currency)
}

/// `int64(Number(lp) * kVoteWeightScaleFactor / T)`, truncated, as a u32.
fn vote_weight(lp: &Number, lpt_balance: &Number) -> i64 {
    lp.mul(&Number::from_int(VOTE_WEIGHT_SCALE_FACTOR))
        .div(lpt_balance)
        .to_i64()
}

fn make_vote_entry(account: &str, fee: i64, vote_weight: i64) -> Value {
    let mut entry = serde_json::json!({
        "Account": account,
        "VoteWeight": vote_weight,
    });
    if fee != 0 {
        entry["TradingFee"] = Value::from(fee);
    }
    serde_json::json!({ "VoteEntry": entry })
}

/// Strict less-than on positive `Number`s (LP balances are non-negative).
fn lp_lt(a: &Number, b: &Number) -> bool {
    !a.sub(b).is_zero() && a.sub(b).negative()
}

fn lp_currency_bytes(amm: &Value) -> Result<[u8; 20], TransactionResult> {
    let hex_str = amm["LPTokenBalance"]["currency"]
        .as_str()
        .ok_or(TransactionResult::TefInternal)?;
    hex::decode(hex_str)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const AMM: &str = "rNhA7WUtVEbZz6xVAPgESEQRUfYCHqGWPg";

    fn lp_cur() -> [u8; 20] {
        let mut c = [0u8; 20];
        c[0] = 0x03;
        c[1] = 0xAB;
        c
    }

    fn seed_lp_line(ledger: &mut Ledger, holder: &str, value: &str) {
        let holder_id = decode_account_id(holder).unwrap();
        let amm_id = decode_account_id(AMM).unwrap();
        let cur = lp_cur();
        let key = keylet::trust_line(&holder_id, &amm_id, &cur);
        let holder_low = holder_id.as_bytes() < amm_id.as_bytes();
        let stored = if holder_low {
            value.to_string()
        } else {
            format!("-{value}")
        };
        let cur_hex = hex::encode_upper(cur);
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": cur_hex, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
            "LowLimit": {"currency": cur_hex, "issuer": if holder_low {holder} else {AMM}, "value": "0"},
            "HighLimit": {"currency": cur_hex, "issuer": if holder_low {AMM} else {holder}, "value": "0"},
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&line).unwrap())
            .unwrap();
    }

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
        let cur_hex = hex::encode_upper(lp_cur());
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Account": AMM,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenBalance": {"currency": cur_hex, "issuer": AMM, "value": "1000"},
            "TradingFee": 500,
            "VoteSlots": [],
            "AuctionSlot": {"Account": ALICE},
            "Flags": 0,
        });
        ledger
            .put_state(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn vote_sets_trading_fee() {
        let mut ledger = setup_with_amm();
        seed_lp_line(&mut ledger, BOB, "1000");
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
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 300);

        let slots = amm["VoteSlots"].as_array().unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["VoteEntry"]["Account"].as_str().unwrap(), BOB);
        assert_eq!(slots[0]["VoteEntry"]["TradingFee"].as_u64().unwrap(), 300);
        assert_eq!(
            slots[0]["VoteEntry"]["VoteWeight"].as_u64().unwrap(),
            100_000
        );
        assert_eq!(amm["AuctionSlot"]["DiscountedFee"].as_u64().unwrap(), 30);
    }

    #[test]
    fn multiple_votes_weighted_average() {
        let mut ledger = setup_with_amm();
        seed_lp_line(&mut ledger, ALICE, "600");
        seed_lp_line(&mut ledger, BOB, "400");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

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
        // (400*600 + 600*400) / (600+400) = 480000 / 1000 = 480.
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 480);
        assert_eq!(amm["VoteSlots"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn non_lp_voter_evicted() {
        let mut ledger = setup_with_amm();
        // ALICE voted once but no longer holds LP (no line). BOB now votes.
        seed_lp_line(&mut ledger, BOB, "1000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // Pre-seed an ALICE vote slot directly by voting while ALICE has LP.
        seed_lp_line_in_sandbox(&mut sandbox, ALICE, "1000");
        let tx1 = serde_json::json!({
            "TransactionType": "AMMVote", "Account": ALICE,
            "Asset": "XRP", "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 100, "Fee": "12", "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx1,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();
        // Now zero out ALICE's LP line.
        zero_lp_line(&mut sandbox, ALICE);

        let tx2 = serde_json::json!({
            "TransactionType": "AMMVote", "Account": BOB,
            "Asset": "XRP", "Asset2": {"currency": "USD", "issuer": BOB},
            "TradingFee": 800, "Fee": "12", "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMVoteTransactor.apply(&mut ctx).unwrap();

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx2).unwrap();
        let amm: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&amm_key).unwrap()).unwrap();
        let slots = amm["VoteSlots"].as_array().unwrap();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0]["VoteEntry"]["Account"].as_str().unwrap(), BOB);
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 800);
    }

    fn seed_lp_line_in_sandbox(sandbox: &mut Sandbox, holder: &str, value: &str) {
        use crate::view::apply_view::ApplyView;
        let holder_id = decode_account_id(holder).unwrap();
        let amm_id = decode_account_id(AMM).unwrap();
        let cur = lp_cur();
        let key = keylet::trust_line(&holder_id, &amm_id, &cur);
        let holder_low = holder_id.as_bytes() < amm_id.as_bytes();
        let stored = if holder_low {
            value.to_string()
        } else {
            format!("-{value}")
        };
        let cur_hex = hex::encode_upper(cur);
        let line = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {"currency": cur_hex, "issuer": "rrrrrrrrrrrrrrrrrrrrBZbvji", "value": stored},
            "LowLimit": {"currency": cur_hex, "issuer": if holder_low {holder} else {AMM}, "value": "0"},
            "HighLimit": {"currency": cur_hex, "issuer": if holder_low {AMM} else {holder}, "value": "0"},
            "Flags": 0,
        });
        sandbox
            .insert(key, serde_json::to_vec(&line).unwrap())
            .unwrap();
    }

    fn zero_lp_line(sandbox: &mut Sandbox, holder: &str) {
        use crate::view::apply_view::ApplyView;
        let holder_id = decode_account_id(holder).unwrap();
        let amm_id = decode_account_id(AMM).unwrap();
        let cur = lp_cur();
        let key = keylet::trust_line(&holder_id, &amm_id, &cur);
        let mut line: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&key).unwrap()).unwrap();
        line["Balance"]["value"] = serde_json::Value::String("0".into());
        sandbox
            .update(key, serde_json::to_vec(&line).unwrap())
            .unwrap();
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
        let mut ledger = setup_with_amm();
        seed_lp_line(&mut ledger, BOB, "1000");
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

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
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
