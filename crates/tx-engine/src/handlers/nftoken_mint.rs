use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
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

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let minter_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // The NFT is owned by the minter (Account); its issuer is the Issuer
        // field when minting on behalf of another account, else the minter.
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap_or(account_str);
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_key = keylet::account(&issuer_id);
        let issuer_bytes = ctx
            .view
            .read(&issuer_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut issuer_acct: Value =
            serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // The token sequence is FirstNFTokenSequence + MintedNFTokens; the first
        // ever mint stamps FirstNFTokenSequence with the ledger sequence.
        let first_seq = match issuer_acct
            .get("FirstNFTokenSequence")
            .and_then(|v| v.as_u64())
        {
            Some(s) => s as u32,
            None => {
                let s = ctx.view.seq();
                issuer_acct["FirstNFTokenSequence"] = Value::from(s);
                s
            }
        };
        let minted = issuer_acct
            .get("MintedNFTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let token_seq = first_seq.wrapping_add(minted);

        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0) as u16;
        let transfer_fee = helpers::get_u32_field(ctx.tx, "TransferFee").unwrap_or(0) as u16;
        let taxon = helpers::get_u32_field(ctx.tx, "NFTokenTaxon").unwrap_or(0);
        let issuer_hex = hex::encode_upper(issuer_id.as_bytes());
        let nftoken_id =
            nftoken::generate_nftoken_id(flags, transfer_fee, &issuer_hex, taxon, token_seq);
        let nft_hash = rxrpl_primitives::Hash256::from_slice(
            &hex::decode(&nftoken_id).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

        // The page NFToken object carries only NFTokenID and (optional) URI; all
        // other attributes are encoded inside the NFTokenID.
        let mut inner = serde_json::json!({ "NFTokenID": nftoken_id });
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            inner["URI"] = Value::String(uri.to_string());
        }
        let entry = serde_json::json!({ "NFToken": inner });
        let new_page = nftoken::insert_token(ctx.view, &minter_id, &nft_hash, entry)?;

        // The issuer's mint count rises; persist before touching the minter so a
        // self-mint (minter == issuer) sees the updated count.
        issuer_acct["MintedNFTokens"] = Value::from(minted + 1);
        let issuer_data =
            serde_json::to_vec(&issuer_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(issuer_key, issuer_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // The minter consumes its sequence proxy; its owner count rises only
        // when a brand-new page had to be created.
        let minter_key = keylet::account(&minter_id);
        let minter_bytes = ctx
            .view
            .read(&minter_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut minter_acct: Value =
            serde_json::from_slice(&minter_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if new_page {
            helpers::adjust_owner_count(&mut minter_acct, 1);
        }
        let minter_data =
            serde_json::to_vec(&minter_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(minter_key, minter_data)
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

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenMintTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify NFTokenPage exists (a first mint creates the owner's last page)
        let minter_id = decode_account_id(MINTER).unwrap();
        let page_key = keylet::nftoken_page_max(&minter_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let tokens = page["NFTokens"].as_array().unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(
            tokens[0]["NFToken"]["NFTokenID"].as_str().unwrap().len(),
            64
        );

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
        let page_key = keylet::nftoken_page_max(&minter_id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let tokens = page["NFTokens"].as_array().unwrap();
        assert_eq!(
            tokens[0]["NFToken"]["URI"].as_str().unwrap(),
            "https://example.com/nft/1"
        );
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
