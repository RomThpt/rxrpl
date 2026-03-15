use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};
use serde_json::Value;

use crate::helpers;
use crate::nftoken;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct NFTokenMintTransactor;

impl Transactor for NFTokenMintTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // NFTokenTaxon must be present
        if helpers::get_u32_field(ctx.tx, "NFTokenTaxon").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        // TransferFee must be <= 50000 (50%) if present
        if let Some(fee) = helpers::get_u32_field(ctx.tx, "TransferFee") {
            if fee > 50000 {
                return Err(TransactionResult::TemBadNFTokenTransfer);
            }
        }

        // URI length <= 256 if present
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            if uri.len() > 256 {
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

        // Read and update source account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let token_seq = helpers::get_sequence(&acct);
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let transfer_fee = helpers::get_u32_field(ctx.tx, "TransferFee").unwrap_or(0) as u16;
        let issuer_hex = hex::encode(account_id.as_bytes()).to_uppercase();

        let nftoken_id = nftoken::generate_nftoken_id(flags, transfer_fee, &issuer_hex, token_seq);

        // Read or create NFTokenPage
        let page_key = keylet::nftoken_page_min(&account_id);
        let mut tokens: Vec<Value> = if let Some(page_bytes) = ctx.view.read(&page_key) {
            let page: Value =
                serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;
            page.get("NFTokens")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Build token object
        let mut token = serde_json::json!({
            "NFTokenID": nftoken_id,
            "Flags": flags,
            "Issuer": account_str,
            "NFTokenTaxon": helpers::get_u32_field(ctx.tx, "NFTokenTaxon").unwrap_or(0),
            "TransferFee": transfer_fee,
        });

        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            token["URI"] = Value::String(uri.to_string());
        }

        tokens.push(token);

        let page_obj = serde_json::json!({
            "LedgerEntryType": "NFTokenPage",
            "NFTokens": tokens,
        });
        let page_data =
            serde_json::to_vec(&page_obj).map_err(|_| TransactionResult::TefInternal)?;

        if ctx.view.exists(&page_key) {
            ctx.view
                .update(page_key, page_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            ctx.view
                .insert(page_key, page_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Update account
        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, 1);

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
    use crate::transactor::PreflightContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const MINTER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_ledger() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(MINTER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": MINTER,
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
    fn mint_basic() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": MINTER,
            "NFTokenTaxon": 0,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenMintTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify NFTokenPage exists
        let minter_id = decode_account_id(MINTER).unwrap();
        let page_key = keylet::nftoken_page_min(&minter_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let tokens = page["NFTokens"].as_array().unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0]["NFTokenID"].as_str().unwrap().len(), 64);

        // Verify owner count incremented
        let acct_key = keylet::account(&minter_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn mint_with_uri() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": MINTER,
            "NFTokenTaxon": 42,
            "URI": "https://example.com/nft/1",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenMintTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let minter_id = decode_account_id(MINTER).unwrap();
        let page_key = keylet::nftoken_page_min(&minter_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let tokens = page["NFTokens"].as_array().unwrap();
        assert_eq!(tokens[0]["URI"].as_str().unwrap(), "https://example.com/nft/1");
        assert_eq!(tokens[0]["NFTokenTaxon"].as_u64().unwrap(), 42);
    }

    #[test]
    fn mint_with_transfer_fee() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": MINTER,
            "NFTokenTaxon": 0,
            "TransferFee": 5000,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenMintTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    #[test]
    fn reject_transfer_fee_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": MINTER,
            "NFTokenTaxon": 0,
            "TransferFee": 60000,
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
            NFTokenMintTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadNFTokenTransfer)
        );
    }

    #[test]
    fn reject_missing_taxon() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": MINTER,
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
            NFTokenMintTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
