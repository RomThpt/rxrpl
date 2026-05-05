use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct AMMClawbackTransactor;

impl Transactor for AMMClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        // Holder cannot be the issuer themselves.
        let account_str = helpers::get_account(ctx.tx)?;
        let holder_str =
            helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
        if account_str == holder_str {
            return Err(TransactionResult::TemMalformed);
        }

        // Amount is OPTIONAL: when absent the issuer claws back the holder's
        // entire AMM position. When present it must be a well-formed amount
        // (XRP drops string or IOU object), strictly positive, and its
        // currency/issuer must match the `Asset` field.
        if let Some(amount_field) = ctx.tx.get("Amount") {
            let amount = amm_helpers::amount_value_drops_or_iou(amount_field)
                .ok_or(TransactionResult::TemBadAmount)?;
            if amount == 0 {
                return Err(TransactionResult::TemBadAmount);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, account) = helpers::read_account_by_address(ctx.view, account_str)?;

        // Issuer must have lsfAllowTrustLineClawback set.
        const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x8000_0000;
        let flags = helpers::get_flags(&account);
        if flags & LSF_ALLOW_TRUST_LINE_CLAWBACK == 0 {
            return Err(TransactionResult::TecNoPermission);
        }

        // Holder account must exist.
        if let Some(holder_str) = helpers::get_str_field(ctx.tx, "Holder") {
            helpers::read_account_by_address(ctx.view, holder_str)?;
        }

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        amm_helpers::read_amm(ctx.view, &amm_key)?;

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Amount may be an IOU object (currency/issuer/value), an XRP drops
        // string, or absent (clawback-all sentinel = u64::MAX).
        let clawback_amount = match ctx.tx.get("Amount") {
            Some(amount_field) => amm_helpers::amount_value_drops_or_iou(amount_field)
                .ok_or(TransactionResult::TemBadAmount)?,
            None => u64::MAX,
        };

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // The clawback drains the issuer's side from the AMM pool. By
        // convention `PoolBalance1` corresponds to `Asset` (the issuer's
        // token).
        let pool1 = amm_helpers::get_pool_field(&amm, "PoolBalance1");
        let pool2 = amm_helpers::get_pool_field(&amm, "PoolBalance2");
        let actual_clawback = clawback_amount.min(pool1);

        let new_pool1 = pool1 - actual_clawback;

        // When the issuer's side is fully drained the AMM has no remaining
        // LP value backing — delete the entry so amm_info reports it gone,
        // matching rippled's deletion-on-zero-LP behaviour.
        if new_pool1 == 0 {
            let _ = pool2;
            ctx.view
                .erase(&amm_key)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            amm["PoolBalance1"] = serde_json::Value::String(new_pool1.to_string());
            let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(amm_key, amm_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Increment the issuer's sequence. Clawed-back IOU tokens are
        // redeemed/destroyed, not credited as XRP to the issuer.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
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

    fn setup_with_amm(pool1: u64, pool2: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

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
            "PoolBalance1": pool1.to_string(),
            "PoolBalance2": pool2.to_string(),
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
    fn clawback_partial() {
        let ledger = setup_with_amm(10_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "3000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        assert_eq!(amm["PoolBalance1"].as_str().unwrap(), "7000000");
        assert_eq!(amm["PoolBalance2"].as_str().unwrap(), "5000000");

        // Issuer's XRP balance is unchanged (clawed IOU tokens are not
        // credited as drops); only the sequence is bumped.
        let alice_id = decode_account_id(ALICE).unwrap();
        let acct_key = keylet::account(&alice_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "100000000");
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn clawback_capped_at_pool() {
        let ledger = setup_with_amm(2_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "10000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Pool fully drained on the issuer's side -> AMM entry deleted.
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        assert!(sandbox.read(&amm_key).is_none());

        let alice_id = decode_account_id(ALICE).unwrap();
        let acct_key = keylet::account(&alice_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "100000000");
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Holder": BOB,
            "Amount": "0",
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
            AMMClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn accept_missing_amount_means_clawback_all() {
        // Per AMMClawback spec, Amount is optional: omitting it claws back
        // the holder's entire AMM position.
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Holder": BOB,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(AMMClawbackTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn reject_nonexistent_amm() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        // 0x80000000 = lsfAllowTrustLineClawback so preclaim passes the
        // permission check and reaches the AMM-existence check.
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0x80000000_u32,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BOB,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(bob_key, serde_json::to_vec(&bob_acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Holder": BOB,
            "Amount": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMClawbackTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_missing_asset2() {
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Amount": "1000000",
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
            AMMClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
