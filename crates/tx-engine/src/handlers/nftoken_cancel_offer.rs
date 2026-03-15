use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};
use rxrpl_primitives::Hash256;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct NFTokenCancelOfferTransactor;

impl Transactor for NFTokenCancelOfferTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let offers = helpers::get_array_field(ctx.tx, "NFTokenOffers")
            .ok_or(TransactionResult::TemMalformed)?;

        if offers.is_empty() {
            return Err(TransactionResult::TemMalformed);
        }

        // Each element must be a 64-char hex string
        for offer in offers {
            let s = offer.as_str().ok_or(TransactionResult::TemMalformed)?;
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let offers = helpers::get_array_field(ctx.tx, "NFTokenOffers")
            .ok_or(TransactionResult::TemMalformed)?
            .clone();

        for offer_id_val in &offers {
            let offer_id_hex = offer_id_val.as_str().unwrap();
            let offer_key_bytes =
                hex::decode(offer_id_hex).map_err(|_| TransactionResult::TemMalformed)?;
            let offer_key = Hash256::from_slice(&offer_key_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;

            let offer_bytes = ctx
                .view
                .read(&offer_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let offer: Value = serde_json::from_slice(&offer_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Verify caller is the offer creator
            let owner = offer
                .get("Owner")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TecNoPermission)?;
            if owner != account_str {
                return Err(TransactionResult::TecNoPermission);
            }

            // Erase the offer
            ctx.view
                .erase(&offer_key)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Adjust owner count for the offer creator
            let owner_id = decode_account_id(owner)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let owner_acct_key = keylet::account(&owner_id);
            let owner_bytes = ctx
                .view
                .read(&owner_acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut owner_acct: Value = serde_json::from_slice(&owner_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut owner_acct, -1);
            let owner_data = serde_json::to_vec(&owner_acct)
                .map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(owner_acct_key, owner_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Increment caller's sequence
        let caller_key = keylet::account(&account_id);
        let caller_bytes = ctx
            .view
            .read(&caller_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut caller: Value =
            serde_json::from_slice(&caller_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut caller);
        let caller_data =
            serde_json::to_vec(&caller).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(caller_key, caller_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::handlers::nftoken_create_offer::NFTokenCreateOfferTransactor;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ACCOUNT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const NFTOKEN_ID: &str = "00000000000000000000000000000000B5F762798A53D543A014CAF8B297CFF8";

    fn setup_with_offer() -> (Ledger, String) {
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

        // Create an offer
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "NFTokenID": NFTOKEN_ID,
            "Amount": "1000000",
            "Flags": 1, // sell
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        NFTokenCreateOfferTransactor.apply(&mut ctx).unwrap();

        let offer_key = keylet::nftoken_offer(&id, 1);
        let offer_id = hex::encode(offer_key.as_bytes()).to_uppercase();

        sandbox.into_changes().apply_to_ledger(&mut ledger).unwrap();
        (ledger, offer_id)
    }

    #[test]
    fn cancel_one_offer() {
        let (ledger, offer_id) = setup_with_offer();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCancelOffer",
            "Account": ACCOUNT,
            "NFTokenOffers": [offer_id],
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenCancelOfferTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Offer should be deleted
        let offer_bytes = hex::decode(&offer_id).unwrap();
        let offer_key = Hash256::from_slice(&offer_bytes).unwrap();
        assert!(sandbox.read(&offer_key).is_none());

        // Owner count should be back to 0
        let acct_id = decode_account_id(ACCOUNT).unwrap();
        let acct_key = keylet::account(&acct_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn reject_empty_offers_array() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCancelOffer",
            "Account": ACCOUNT,
            "NFTokenOffers": [],
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
            NFTokenCancelOfferTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
