use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{dir_remove, remove_from_owner_dir_page};
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

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
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
            let offer: Value =
                serde_json::from_slice(&offer_bytes).map_err(|_| TransactionResult::TefInternal)?;

            // The offer's creator OR its named destination may cancel it.
            let owner = offer
                .get("Owner")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TecNoPermission)?;
            let is_destination = offer
                .get("Destination")
                .and_then(|v| v.as_str())
                .map(|d| d == account_str)
                .unwrap_or(false);
            if owner != account_str && !is_destination {
                return Err(TransactionResult::TecNoPermission);
            }

            let owner_id =
                decode_account_id(owner).map_err(|_| TransactionResult::TemInvalidAccountId)?;

            // Unlink from the owner directory (page recorded as OwnerNode) and
            // from the per-NFToken buy/sell offer book, then erase the offer.
            let owner_node = offer
                .get("OwnerNode")
                .and_then(|v| v.as_str())
                .and_then(|s| u64::from_str_radix(s, 16).ok())
                .unwrap_or(0);
            remove_from_owner_dir_page(ctx.view, &owner_id, owner_node, &offer_key)?;

            let is_sell = offer.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) & 1 != 0;
            if let Some(nft_id_hex) = offer.get("NFTokenID").and_then(|v| v.as_str()) {
                if let Ok(nft_bytes) = hex::decode(nft_id_hex) {
                    if let Ok(nft_hash) = Hash256::from_slice(&nft_bytes) {
                        let book = if is_sell {
                            keylet::nft_sells(&nft_hash)
                        } else {
                            keylet::nft_buys(&nft_hash)
                        };
                        dir_remove(ctx.view, &book, &offer_key)?;
                    }
                }
            }

            ctx.view
                .erase(&offer_key)
                .map_err(|_| TransactionResult::TefInternal)?;

            // A Destination-restricted offer touches the destination AccountRoot
            // (its PreviousTxnID is threaded though no field changes).
            if let Some(dest) = offer.get("Destination").and_then(|v| v.as_str()) {
                if let Ok(dest_id) = decode_account_id(dest) {
                    let dest_key = keylet::account(&dest_id);
                    if let Some(dest_bytes) = ctx.view.read(&dest_key) {
                        ctx.view
                            .update(dest_key, dest_bytes)
                            .map_err(|_| TransactionResult::TefInternal)?;
                    }
                }
            }

            // Adjust owner count for the offer creator
            let owner_acct_key = keylet::account(&owner_id);
            let owner_bytes = ctx
                .view
                .read(&owner_acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut owner_acct: Value =
                serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut owner_acct, -1);
            let owner_data =
                serde_json::to_vec(&owner_acct).map_err(|_| TransactionResult::TefInternal)?;
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
        let caller: Value =
            serde_json::from_slice(&caller_bytes).map_err(|_| TransactionResult::TefInternal)?;
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
        // The seller must own the NFT for the sell offer to be placed
        // (NFTokenCreateOffer::preclaim findToken).
        let nft: rxrpl_primitives::Hash256 = NFTOKEN_ID.parse().unwrap();
        crate::nftoken::insert_token(
            &mut sandbox,
            &id,
            &nft,
            serde_json::json!({ "NFToken": { "NFTokenID": NFTOKEN_ID } }),
        )
        .unwrap();
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
