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

        // The minter consumes its sequence proxy; its owner count rises when a
        // brand-new NFTokenPage had to be created and/or an atomic sell offer is
        // created (NFTokenMintOffer amendment).
        let minter_key = keylet::account(&minter_id);
        let minter_bytes = ctx
            .view
            .read(&minter_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut minter_acct: Value =
            serde_json::from_slice(&minter_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // rippled NFTokenMint::doApply captures ownerCountBefore *after* the
        // central Sequence/Ticket consume (already applied; a consumed ticket has
        // decremented OwnerCount by one). Adding a brand-new page reserves one
        // object; adding a token to an existing, non-full page reserves nothing.
        let owner_count_before = helpers::get_owner_count(&minter_acct);
        if new_page {
            helpers::adjust_owner_count(&mut minter_acct, 1);
        }

        // NFTokenMintOffer amendment (rippled NFTokenMint::doApply): when the mint
        // carries sfAmount it ALSO atomically creates a sell NFTokenOffer for the
        // just-minted token (optionally Destination/Expiration-restricted). The
        // missing offer object, its directory links and the +1 owner count are
        // the state divergence this fixes. rippled shares the offer-creation code
        // with NFTokenCreateOffer (`nft::tokenOfferCreateApply`, always passing
        // tfSellNFToken — a mint may only create a sell offer).
        if ctx.tx.get("Amount").is_some() {
            self.create_sell_offer(
                ctx,
                &minter_id,
                account_str,
                &nftoken_id,
                &nft_hash,
                &mut minter_acct,
            )?;
        }

        // Owner reserve (rippled NFTokenMint::doApply tail): checked ONLY when the
        // owner count actually rose, comparing `accountReserve(ownerCountAfter)`
        // against `mPriorBalance` (the XRP balance *before* the fee). The engine
        // deducted the fee centrally before doApply, so reconstruct mPriorBalance
        // by adding it back. tecINSUFFICIENT_RESERVE is a CLAIMED tec — fee and
        // sequence charged, all child writes (the new page, the issuer mint count,
        // the offer) discarded by the engine.
        let owner_count_after = helpers::get_owner_count(&minter_acct);
        if owner_count_after > owner_count_before {
            let prior_balance =
                helpers::get_balance(&minter_acct).saturating_add(helpers::get_fee(ctx.tx));
            if prior_balance < ctx.fees.account_reserve(owner_count_after) {
                return Err(TransactionResult::TecInsufficientReserve);
            }
        }
        let minter_data =
            serde_json::to_vec(&minter_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(minter_key, minter_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// True when an offer amount is zero (XRP `"0"` drops or IOU value `0`). A
/// zero-amount (gift) sell offer omits sfAmount, matching rippled's default-drop
/// serialization. Mirrors the helper in `nftoken_create_offer`.
fn amount_is_zero(amount: &Value) -> bool {
    match amount {
        Value::String(s) => s.parse::<u128>().map(|n| n == 0).unwrap_or(false),
        Value::Object(o) => o
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| {
                s.trim_start_matches('-')
                    .trim_matches('0')
                    .trim_matches('.')
                    .is_empty()
            })
            .unwrap_or(false),
        _ => false,
    }
}

impl NFTokenMintTransactor {
    /// NFTokenMintOffer: atomically create the sell NFTokenOffer for the token
    /// just minted. Mirrors rippled `nft::tokenOfferCreateApply` with the
    /// tfSellNFToken path — owner-directory and per-NFToken sell-book links, the
    /// NFTokenOffer SLE (always `lsfSellNFToken` = 1), a +1 owner-count bump on
    /// the minter, and the Destination AccountRoot touch. The offer keylet uses
    /// the transaction's seq-proxy (the ticket sequence for a ticketed mint),
    /// matching the on-chain NFTokenOffer index.
    #[allow(clippy::too_many_arguments)]
    fn create_sell_offer(
        &self,
        ctx: &mut ApplyContext<'_>,
        minter_id: &rxrpl_primitives::AccountId,
        minter_str: &str,
        nftoken_id: &str,
        nft_hash: &rxrpl_primitives::Hash256,
        minter_acct: &mut Value,
    ) -> Result<(), TransactionResult> {
        let tx_seq = helpers::tx_seq_proxy_value(ctx.tx);
        let offer_key = keylet::nftoken_offer(minter_id, tx_seq);

        // Owner directory link + per-NFToken sell-offer book link.
        let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, minter_id, &offer_key)?;
        let book_key = keylet::nft_sells(nft_hash);
        crate::owner_dir::add_to_nft_offer_dir(ctx.view, &book_key, nftoken_id, &offer_key, true)?;

        let mut offer = serde_json::json!({
            "LedgerEntryType": "NFTokenOffer",
            "Owner": minter_str,
            "NFTokenID": nftoken_id,
            "Flags": 1u32,
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        let amount_value = ctx
            .tx
            .get("Amount")
            .cloned()
            .unwrap_or_else(|| Value::String("0".to_string()));
        if !amount_is_zero(&amount_value) {
            offer["Amount"] = amount_value;
        }
        // OwnerNode is omitted on the directory's root page (node 0).
        if owner_node != 0 {
            offer["OwnerNode"] = Value::from(format!("{owner_node:016X}"));
        }
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

        // The offer adds one owned object to the minter.
        helpers::adjust_owner_count(minter_acct, 1);

        // A Destination-restricted offer touches the destination AccountRoot (its
        // PreviousTxnID is threaded though no field changes), as rippled does when
        // validating the named destination.
        if let Some(dest) = helpers::get_str_field(ctx.tx, "Destination") {
            if let Ok(dest_id) = decode_account_id(dest) {
                let dest_key = keylet::account(&dest_id);
                if let Some(dest_bytes) = ctx.view.read(&dest_key) {
                    ctx.view
                        .update(dest_key, dest_bytes)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
            }
        }
        Ok(())
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
