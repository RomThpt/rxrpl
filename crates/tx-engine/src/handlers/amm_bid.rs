use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode, power};
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

const AUCTION_SLOT_DISCOUNTED_FEE_FRACTION: i64 = 10;
const AUCTION_SLOT_MIN_FEE_FRACTION: i64 = 25;
const AUCTION_SLOT_TIME_INTERVALS: u64 = 20;
const AUCTION_SLOT_MAX_AUTH_ACCOUNTS: usize = 4;
const TOTAL_TIME_SLOT_SECS: u64 = 86_400;
const AUCTION_SLOT_INTERVAL_DURATION: u64 = TOTAL_TIME_SLOT_SECS / AUCTION_SLOT_TIME_INTERVALS;
const TAILING_SLOT: u8 = (AUCTION_SLOT_TIME_INTERVALS - 1) as u8;

pub struct AMMBidTransactor;

impl Transactor for AMMBidTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        if let Some(bid_min) = ctx.tx.get("BidMin") {
            if !amm_helpers::amount_is_positive(bid_min) {
                return Err(TransactionResult::TemBadAmount);
            }
        }
        if let Some(bid_max) = ctx.tx.get("BidMax") {
            if !amm_helpers::amount_is_positive(bid_max) {
                return Err(TransactionResult::TemBadAmount);
            }
        }

        if let Some(auth) = ctx.tx.get("AuthAccounts").and_then(|v| v.as_array()) {
            if auth.len() > AUCTION_SLOT_MAX_AUTH_ACCOUNTS {
                return Err(TransactionResult::TemMalformed);
            }
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

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        let amm_account = decode_account_id(
            amm["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let lp_currency = lp_currency_bytes(&amm)?;

        // T = total outstanding LP tokens (LPTokenBalance).
        let t_iou = amm_helpers::parse_iou_value(
            amm["LPTokenBalance"]["value"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        );
        let t = Number::from_iou(&t_iou);

        // The bidder's own LP holding.
        let lp_tokens =
            amm_helpers::iou_holding_number(ctx.view, &account_id, &amm_account, &lp_currency);

        let trading_fee = amm["TradingFee"].as_u64().unwrap_or(0);
        let discounted_fee = (trading_fee as i64) / AUCTION_SLOT_DISCOUNTED_FEE_FRACTION;
        let fee = Number::from_int(trading_fee as i64).div(&Number::from_int(100_000));
        let min_slot_price = t
            .mul(&fee)
            .div(&Number::from_int(AUCTION_SLOT_MIN_FEE_FRACTION));

        let current = ctx.view.parent_close_time() as u64;
        let expiration = amm["AuctionSlot"]
            .get("Expiration")
            .and_then(|v| v.as_u64());
        let time_slot = expiration.and_then(|exp| amm_auction_time_slot(current, exp));

        let slot_account = amm["AuctionSlot"]
            .get("Account")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let valid_owner = match (&slot_account, time_slot) {
            (Some(acct), Some(ts)) if ts < TAILING_SLOT => decode_account_id(acct)
                .ok()
                .map(|id| ctx.view.exists(&keylet::account(&id)))
                .unwrap_or(false),
            _ => false,
        };

        let bid_min = bid_value(ctx.tx, "BidMin");
        let bid_max = bid_value(ctx.tx, "BidMax");

        let get_pay_price = |computed: &Number| -> Result<Number, TransactionResult> {
            let pay = match (&bid_min, &bid_max) {
                (Some(lo), Some(hi)) => {
                    if !gt(computed, hi) {
                        max(computed, lo)
                    } else {
                        return Err(TransactionResult::TecAmmFailed);
                    }
                }
                (Some(lo), None) => max(computed, lo),
                (None, Some(hi)) => {
                    if !gt(computed, hi) {
                        *computed
                    } else {
                        return Err(TransactionResult::TecAmmFailed);
                    }
                }
                (None, None) => *computed,
            };
            if gt(&pay, &lp_tokens) {
                return Err(TransactionResult::TecAmmInvalidTokens);
            }
            Ok(pay)
        };

        let (pay_price, burn, refund) = if slot_account.is_none() || !valid_owner {
            let pay = get_pay_price(&min_slot_price)?;
            (pay, pay, None)
        } else {
            let price_purchased = amm_helpers::parse_iou_value(
                amm["AuctionSlot"]["Price"]["value"].as_str().unwrap_or("0"),
            );
            let price_purchased = Number::from_iou(&price_purchased);
            let ts = time_slot.ok_or(TransactionResult::TefInternal)?;
            let fraction_used = Number::from_int(ts as i64 + 1)
                .div(&Number::from_int(AUCTION_SLOT_TIME_INTERVALS as i64));
            let fraction_remaining = Number::from_int(1).sub(&fraction_used);
            let p105 = Number::new(false, 105, -2);
            let computed = if ts == 0 {
                price_purchased.mul(&p105).add(&min_slot_price)
            } else {
                let decay = Number::from_int(1).sub(&power(&fraction_used, 60));
                price_purchased.mul(&p105).mul(&decay).add(&min_slot_price)
            };
            let pay = get_pay_price(&computed)?;
            let refund = fraction_remaining.mul(&price_purchased);
            if gt(&refund, &pay) {
                return Err(TransactionResult::TefInternal);
            }
            let burn = pay.sub(&refund);
            (pay, burn, Some(refund))
        };

        // Refund the previous owner (CASE B): send LP from bidder to the old
        // slot account.
        if let Some(refund) = refund {
            let prev_owner = decode_account_id(
                slot_account
                    .as_deref()
                    .ok_or(TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
            let refund_iou = refund.to_iou();
            let refund_num = Number::from_iou(&refund_iou);
            let bidder_hold =
                amm_helpers::iou_holding_number(ctx.view, &account_id, &amm_account, &lp_currency);
            amm_helpers::set_iou_holding(
                ctx.view,
                &account_id,
                &amm_account,
                &lp_currency,
                &bidder_hold.sub(&refund_num),
            )?;
            let owner_hold =
                amm_helpers::iou_holding_number(ctx.view, &prev_owner, &amm_account, &lp_currency);
            amm_helpers::set_iou_holding(
                ctx.view,
                &prev_owner,
                &amm_account,
                &lp_currency,
                &owner_hold.add(&refund_num),
            )?;
        }

        // updateSlot: rewrite the auction slot fields.
        let new_expiration = current + TOTAL_TIME_SLOT_SECS;
        let lp_currency_hex = amm["LPTokenBalance"]["currency"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let amm_account_str = amm["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        let mut auction = serde_json::Map::new();
        auction.insert("Account".into(), Value::String(account_str.to_string()));
        if discounted_fee != 0 {
            auction.insert("DiscountedFee".into(), Value::from(discounted_fee));
        }
        auction.insert("Expiration".into(), Value::from(new_expiration));
        auction.insert(
            "Price".into(),
            serde_json::json!({
                "currency": lp_currency_hex,
                "issuer": amm_account_str,
                "value": pay_price.to_iou().to_decimal_string(),
            }),
        );
        if let Some(auth) = ctx.tx.get("AuthAccounts") {
            auction.insert("AuthAccounts".into(), auth.clone());
        }
        amm["AuctionSlot"] = Value::Object(auction);

        // saBurn = adjustLPTokens(T, burn, IsDeposit::No) = (burn - T) + T under
        // Downward.
        let burn_iou = burn.to_iou();
        let sa_burn = {
            let _g = RoundModeGuard::new(RoundingMode::Downward);
            Number::from_iou(&burn_iou).sub(&t).add(&t).to_iou()
        };
        let sa_burn_num = Number::from_iou(&sa_burn);

        // Redeem saBurn LP from the bidder's holding.
        let bidder_hold =
            amm_helpers::iou_holding_number(ctx.view, &account_id, &amm_account, &lp_currency);
        amm_helpers::set_iou_holding(
            ctx.view,
            &account_id,
            &amm_account,
            &lp_currency,
            &bidder_hold.sub(&sa_burn_num),
        )?;

        // AMM.LPTokenBalance = T - saBurn.
        let new_t = t.sub(&sa_burn_num).to_iou();
        amm["LPTokenBalance"]["value"] = Value::String(new_t.to_decimal_string());

        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Bump the bidder's sequence (fee charged by the engine).
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// `ammAuctionTimeSlot`: the current time slot (0..19) for an auction slot whose
/// `Expiration` is given, or `None` if outside the auction window.
fn amm_auction_time_slot(current: u64, expiration: u64) -> Option<u8> {
    if expiration < TOTAL_TIME_SLOT_SECS {
        return None;
    }
    let start = expiration - TOTAL_TIME_SLOT_SECS;
    if current >= start {
        let diff = current - start;
        if diff < TOTAL_TIME_SLOT_SECS {
            return Some((diff / AUCTION_SLOT_INTERVAL_DURATION) as u8);
        }
    }
    None
}

/// A BidMin/BidMax field as a `Number` (LP-token IOU value), if present.
fn bid_value(tx: &Value, field: &str) -> Option<Number> {
    let v = tx.get(field)?;
    let s = v.get("value").and_then(|x| x.as_str())?;
    Some(Number::from_iou(&amm_helpers::parse_iou_value(s)))
}

/// Strict greater-than on `Number`s.
fn gt(a: &Number, b: &Number) -> bool {
    let d = a.sub(b);
    !d.is_zero() && !d.negative()
}

/// `max(a, b)` on `Number`s.
fn max(a: &Number, b: &Number) -> Number {
    if gt(b, a) { *b } else { *a }
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
    use crate::transactor::PreflightContext;
    use rxrpl_amendment::Rules;

    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    #[test]
    fn reject_zero_bid_min() {
        let tx = serde_json::json!({
            "TransactionType": "AMMBid",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "BidMin": {"currency": "USD", "issuer": BOB, "value": "0"},
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
            AMMBidTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_missing_asset() {
        let tx = serde_json::json!({
            "TransactionType": "AMMBid",
            "Account": BOB,
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
            AMMBidTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_too_many_auth_accounts() {
        let tx = serde_json::json!({
            "TransactionType": "AMMBid",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "AuthAccounts": [
                {"AuthAccount": {"Account": BOB}},
                {"AuthAccount": {"Account": BOB}},
                {"AuthAccount": {"Account": BOB}},
                {"AuthAccount": {"Account": BOB}},
                {"AuthAccount": {"Account": BOB}},
            ],
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
            AMMBidTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn time_slot_window() {
        // expiration = start + TOTAL; current at start → slot 0.
        let exp = TOTAL_TIME_SLOT_SECS + 1000;
        assert_eq!(amm_auction_time_slot(1000, exp), Some(0));
        // one interval in.
        assert_eq!(
            amm_auction_time_slot(1000 + AUCTION_SLOT_INTERVAL_DURATION, exp),
            Some(1)
        );
        // at/after expiration → None.
        assert_eq!(
            amm_auction_time_slot(1000 + TOTAL_TIME_SLOT_SECS, exp),
            None
        );
        // before window → None.
        assert_eq!(amm_auction_time_slot(999, exp), None);
    }
}
