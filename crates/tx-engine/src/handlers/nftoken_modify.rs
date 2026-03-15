use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct NFTokenModifyTransactor;

impl Transactor for NFTokenModifyTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let id = helpers::get_str_field(ctx.tx, "NFTokenID")
            .ok_or(TransactionResult::TemMalformed)?;
        if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TransactionResult::TemMalformed);
        }

        // Must have at least URI to modify
        if helpers::get_str_field(ctx.tx, "URI").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Verify token exists in account's page
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let page_key = keylet::nftoken_page_min(&account_id);
        let page_bytes = ctx.view.read(&page_key).ok_or(TransactionResult::TecNoEntry)?;
        let page: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID").unwrap();
        let tokens = page
            .get("NFTokens")
            .and_then(|v| v.as_array())
            .ok_or(TransactionResult::TecNoEntry)?;

        let found = tokens.iter().any(|t| {
            t.get("NFTokenID")
                .and_then(|v| v.as_str())
                .map(|s| s == nftoken_id)
                .unwrap_or(false)
        });
        if !found {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID").unwrap();

        // Update token in page
        let page_key = keylet::nftoken_page_min(&account_id);
        let page_bytes = ctx
            .view
            .read(&page_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let page: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let mut tokens: Vec<Value> = page
            .get("NFTokens")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for token in &mut tokens {
            if token
                .get("NFTokenID")
                .and_then(|v| v.as_str())
                .map(|s| s == nftoken_id)
                .unwrap_or(false)
            {
                if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
                    token["URI"] = Value::String(uri.to_string());
                }
                break;
            }
        }

        let page_obj = serde_json::json!({
            "LedgerEntryType": "NFTokenPage",
            "NFTokens": tokens,
        });
        let page_data =
            serde_json::to_vec(&page_obj).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(page_key, page_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment sequence
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut acct);
        let acct_data =
            serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
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
    use crate::handlers::nftoken_mint::NFTokenMintTransactor;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_with_token() -> (Ledger, String) {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": OWNER,
            "NFTokenTaxon": 0,
            "URI": "https://old-uri.com",
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        NFTokenMintTransactor.apply(&mut ctx).unwrap();

        let page_key = keylet::nftoken_page_min(&id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let nftoken_id = page["NFTokens"][0]["NFTokenID"]
            .as_str()
            .unwrap()
            .to_string();

        sandbox.into_changes().apply_to_ledger(&mut ledger).unwrap();
        (ledger, nftoken_id)
    }

    #[test]
    fn modify_uri() {
        let (ledger, nftoken_id) = setup_with_token();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenModify",
            "Account": OWNER,
            "NFTokenID": nftoken_id,
            "URI": "https://new-uri.com",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenModifyTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify URI updated
        let owner_id = decode_account_id(OWNER).unwrap();
        let page_key = keylet::nftoken_page_min(&owner_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        assert_eq!(
            page["NFTokens"][0]["URI"].as_str().unwrap(),
            "https://new-uri.com"
        );
    }

    #[test]
    fn reject_missing_uri() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenModify",
            "Account": OWNER,
            "NFTokenID": "00000000000000000000000000000000B5F762798A53D543A014CAF8B297CFF8",
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
            NFTokenModifyTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_nonexistent_token() {
        let (ledger, _) = setup_with_token();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenModify",
            "Account": OWNER,
            "NFTokenID": "0000000000000000000000000000000000000000000000000000000000000000",
            "URI": "https://nope.com",
            "Fee": "12",
        });
        let ctx = crate::transactor::PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            NFTokenModifyTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }
}
