use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct NFTokenBurnTransactor;

impl Transactor for NFTokenBurnTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let id =
            helpers::get_str_field(ctx.tx, "NFTokenID").ok_or(TransactionResult::TemMalformed)?;
        if id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Determine which account owns the token
        let owner_str = helpers::get_str_field(ctx.tx, "Owner").unwrap_or(account_str);
        let owner_id =
            decode_account_id(owner_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Check token exists in owner's page
        let page_key = keylet::nftoken_page_min(&owner_id);
        let page_bytes = ctx
            .view
            .read(&page_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let page: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID").unwrap();
        let tokens = page
            .get("NFTokens")
            .and_then(|v| v.as_array())
            .ok_or(TransactionResult::TecNoEntry)?;

        let token = tokens.iter().find(|t| {
            t.get("NFTokenID")
                .and_then(|v| v.as_str())
                .map(|s| s == nftoken_id)
                .unwrap_or(false)
        });
        let token = token.ok_or(TransactionResult::TecNoEntry)?;

        // Permission: caller can always burn its own NFTs. If caller is
        // attempting to burn someone else's NFT (typical issuer flow), the
        // NFT must have the lsfBurnable flag (0x0001) set.
        if account_str != owner_str {
            const LSF_BURNABLE: u32 = 0x0001;
            let nft_flags = token.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            if nft_flags & LSF_BURNABLE == 0 {
                return Err(TransactionResult::TecNoPermission);
            }
            // Only the original issuer (encoded in NFTokenID bytes 16..56)
            // may invoke the burnable-by-issuer path.
            let issuer_hex_in_id = &nftoken_id[16..56];
            let account_id_bytes =
                decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let account_hex = hex::encode_upper(account_id_bytes.as_bytes());
            if !issuer_hex_in_id.eq_ignore_ascii_case(&account_hex) {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let owner_str = helpers::get_str_field(ctx.tx, "Owner").unwrap_or(account_str);
        let owner_id =
            decode_account_id(owner_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID").unwrap();

        // Remove token from page
        let page_key = keylet::nftoken_page_min(&owner_id);
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

        tokens.retain(|t| {
            t.get("NFTokenID")
                .and_then(|v| v.as_str())
                .map(|s| s != nftoken_id)
                .unwrap_or(true)
        });

        if tokens.is_empty() {
            // Delete page if empty
            ctx.view
                .erase(&page_key)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            let page_obj = serde_json::json!({
                "LedgerEntryType": "NFTokenPage",
                "NFTokens": tokens,
            });
            let page_data =
                serde_json::to_vec(&page_obj).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(page_key, page_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Update owner account
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

        // Update caller account (increment sequence)
        let caller_key = keylet::account(&account_id);
        let caller_bytes = ctx
            .view
            .read(&caller_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut caller: Value =
            serde_json::from_slice(&caller_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut caller);

        // If caller is different from owner, update separately
        if account_id != owner_id {
            let caller_data =
                serde_json::to_vec(&caller).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(caller_key, caller_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Already updated owner, but need to re-read since we modified it
            // Actually the owner_acct was already written, re-read and update sequence
            let updated_bytes = ctx
                .view
                .read(&caller_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut updated: Value = serde_json::from_slice(&updated_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut updated);
            let data = serde_json::to_vec(&updated).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(caller_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

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

        // Mint a token
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenMint",
            "Account": OWNER,
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
        NFTokenMintTransactor.apply(&mut ctx).unwrap();

        // Get the token ID
        let page_key = keylet::nftoken_page_min(&id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let nftoken_id = page["NFTokens"][0]["NFTokenID"]
            .as_str()
            .unwrap()
            .to_string();

        // Apply sandbox to ledger
        sandbox.into_changes().apply_to_ledger(&mut ledger).unwrap();

        (ledger, nftoken_id)
    }

    #[test]
    fn burn_own_token() {
        let (ledger, nftoken_id) = setup_with_token();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenBurn",
            "Account": OWNER,
            "NFTokenID": nftoken_id,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = NFTokenBurnTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Page should be deleted (was the only token)
        let owner_id = decode_account_id(OWNER).unwrap();
        let page_key = keylet::nftoken_page_min(&owner_id);
        assert!(sandbox.read(&page_key).is_none());
    }

    #[test]
    fn burn_nonexistent_token_fails() {
        let (ledger, _) = setup_with_token();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenBurn",
            "Account": OWNER,
            "NFTokenID": "0000000000000000B5F762798A53D543A014CAF8B297CFF8F2F937E8DEADBEEF",
            "Fee": "12",
            "Sequence": 2,
        });
        let ctx = crate::transactor::PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };

        assert_eq!(
            NFTokenBurnTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn preflight_invalid_id() {
        let tx = serde_json::json!({
            "TransactionType": "NFTokenBurn",
            "Account": OWNER,
            "NFTokenID": "too_short",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(NFTokenBurnTransactor.preflight(&ctx).is_err());
    }
}
