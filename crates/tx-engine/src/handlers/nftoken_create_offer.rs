use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// tfSellNFToken flag
const TF_SELL_NFTOKEN: u32 = 0x0001;

pub struct NFTokenCreateOfferTransactor;

impl Transactor for NFTokenCreateOfferTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // NFTokenID must be present and valid
        let id =
            helpers::get_str_field(ctx.tx, "NFTokenID").ok_or(TransactionResult::TemMalformed)?;
        if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TransactionResult::TemMalformed);
        }

        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let is_sell = flags & TF_SELL_NFTOKEN != 0;

        // Buy offers must have Amount > 0
        if !is_sell {
            let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
            if amount == 0 {
                return Err(TransactionResult::TemBadAmount);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Read and update account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let tx_seq = helpers::get_sequence(&acct);
        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, 1);

        let acct_data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create NFTokenOffer entry
        let offer_key = keylet::nftoken_offer(&account_id, tx_seq);
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID").unwrap();
        let amount = helpers::get_xrp_amount(ctx.tx).unwrap_or(0);

        let mut offer = serde_json::json!({
            "LedgerEntryType": "NFTokenOffer",
            "Owner": account_str,
            "NFTokenID": nftoken_id,
            "Amount": amount.to_string(),
            "Flags": flags,
            "Sequence": tx_seq,
        });

        if let Some(dest) = helpers::get_str_field(ctx.tx, "Destination") {
            offer["Destination"] = Value::String(dest.to_string());
        }
        if let Some(exp) = helpers::get_u32_field(ctx.tx, "Expiration") {
            offer["Expiration"] = Value::from(exp);
        }

        let offer_data = serde_json::to_vec(&offer).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(offer_key, offer_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        add_to_owner_dir(ctx.view, &account_id, &offer_key)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::PreflightContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ACCOUNT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const NFTOKEN_ID: &str = "00000000000000000000000000000000B5F762798A53D543A014CAF8B297CFF8";

    fn setup_ledger() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ACCOUNT).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ACCOUNT,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn create_sell_offer() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "NFTokenID": NFTOKEN_ID,
            "Amount": "1000000",
            "Flags": TF_SELL_NFTOKEN,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenCreateOfferTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify offer exists
        let acct_id = decode_account_id(ACCOUNT).unwrap();
        let offer_key = keylet::nftoken_offer(&acct_id, 1);
        let offer_bytes = sandbox.read(&offer_key).unwrap();
        let offer: Value = serde_json::from_slice(&offer_bytes).unwrap();
        assert_eq!(offer["NFTokenID"].as_str().unwrap(), NFTOKEN_ID);
        assert_eq!(offer["Flags"].as_u64().unwrap(), TF_SELL_NFTOKEN as u64);
    }

    #[test]
    fn create_buy_offer() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "NFTokenID": NFTOKEN_ID,
            "Amount": "5000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenCreateOfferTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    #[test]
    fn reject_zero_amount_buy_offer() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "NFTokenID": NFTOKEN_ID,
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
            NFTokenCreateOfferTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }
}
