use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::nftoken;
use crate::owner_dir::{dir_remove_page, remove_from_owner_dir_page};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

fn nft_hash(id_hex: &str) -> Result<Hash256, TransactionResult> {
    let bytes = hex::decode(id_hex).map_err(|_| TransactionResult::TemMalformed)?;
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

/// The issuer AccountID encoded in the NFTokenID (hex chars 8..48).
fn issuer_from_id(nftoken_id: &str) -> Option<rxrpl_primitives::AccountId> {
    let bytes = hex::decode(nftoken_id.get(8..48)?).ok()?;
    rxrpl_primitives::AccountId::from_slice(&bytes).ok()
}

/// Burn removes sell offers first, then buy offers, up to the protocol cap.
/// Preserve directory page order and remove each page's entries in reverse,
/// as rippled's `removeTokenOffersWithLimit` does.
fn remove_token_offers_with_limit(
    ctx: &mut ApplyContext<'_>,
    book: &Hash256,
    limit: usize,
) -> Result<usize, TransactionResult> {
    let mut removed = 0;
    let mut page = 0;
    while removed < limit {
        let page_key = keylet::dir_node(book, page);
        let Some(page_bytes) = ctx.view.read(&page_key) else {
            break;
        };
        let directory: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next_page = directory
            .get("IndexNext")
            .and_then(Value::as_str)
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .unwrap_or(0);
        let indexes = directory
            .get("Indexes")
            .and_then(Value::as_array)
            .ok_or(TransactionResult::TefInternal)?;
        for index in indexes.iter().rev() {
            let offer_key = index
                .as_str()
                .and_then(|value| value.parse::<Hash256>().ok())
                .ok_or(TransactionResult::TefInternal)?;
            let Some(offer_bytes) = ctx.view.read(&offer_key) else {
                continue;
            };
            let offer: Value =
                serde_json::from_slice(&offer_bytes).map_err(|_| TransactionResult::TefInternal)?;
            if offer.get("LedgerEntryType").and_then(Value::as_str) != Some("NFTokenOffer") {
                return Err(TransactionResult::TefInternal);
            }
            let owner = offer
                .get("Owner")
                .and_then(Value::as_str)
                .ok_or(TransactionResult::TefInternal)?;
            let owner_id = decode_account_id(owner).map_err(|_| TransactionResult::TefInternal)?;
            let node = |field: &str| {
                offer
                    .get(field)
                    .and_then(Value::as_str)
                    .and_then(|value| u64::from_str_radix(value, 16).ok())
                    .ok_or(TransactionResult::TefInternal)
            };
            remove_from_owner_dir_page(ctx.view, &owner_id, node("OwnerNode")?, &offer_key)?;
            dir_remove_page(ctx.view, book, node("NFTokenOfferNode")?, &offer_key)?;
            ctx.view
                .erase(&offer_key)
                .map_err(|_| TransactionResult::TefInternal)?;

            let owner_key = keylet::account(&owner_id);
            let owner_bytes = ctx
                .view
                .read(&owner_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut owner_acct: Value =
                serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut owner_acct, -1);
            ctx.view
                .update(
                    owner_key,
                    serde_json::to_vec(&owner_acct).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;

            // A restricted offer threads its named destination AccountRoot
            // when the offer is deleted, even though no account field changes.
            if let Some(destination) = offer.get("Destination").and_then(Value::as_str) {
                let destination_id =
                    decode_account_id(destination).map_err(|_| TransactionResult::TefInternal)?;
                let destination_key = keylet::account(&destination_id);
                if let Some(destination_bytes) = ctx.view.read(&destination_key) {
                    ctx.view
                        .update(destination_key, destination_bytes)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
            }

            removed += 1;
            if removed == limit {
                break;
            }
        }
        if next_page == 0 {
            break;
        }
        page = next_page;
    }
    Ok(removed)
}

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

        let nftoken_id =
            helpers::get_str_field(ctx.tx, "NFTokenID").ok_or(TransactionResult::TemMalformed)?;
        let nft_hash = nft_hash(nftoken_id)?;

        // Check the token exists in one of the owner's pages.
        let page_key = nftoken::find_owner_page(ctx.view, &owner_id, &nft_hash)
            .ok_or(TransactionResult::TecNoEntry)?;
        let page_bytes = ctx
            .view
            .read(&page_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let page: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let tokens = page
            .get("NFTokens")
            .and_then(|v| v.as_array())
            .ok_or(TransactionResult::TecNoEntry)?;

        let exists = tokens
            .iter()
            .any(|t| nftoken::entry_nftoken_id(t) == Some(nftoken_id));
        if !exists {
            return Err(TransactionResult::TecNoEntry);
        }

        // Permission: caller can always burn its own NFTs. If caller is
        // attempting to burn someone else's NFT (typical issuer flow), the
        // NFT must have the lsfBurnable flag (0x0001), encoded in the ID.
        if account_str != owner_str {
            const LSF_BURNABLE: u32 = 0x0001;
            let nft_flags = u32::from_str_radix(&nftoken_id[0..4], 16).unwrap_or(0);
            if nft_flags & LSF_BURNABLE == 0 {
                return Err(TransactionResult::TecNoPermission);
            }
            // Only the original issuer (encoded in NFTokenID bytes 4..24,
            // i.e. hex 8..48) may invoke the burnable-by-issuer path.
            let issuer_hex_in_id = &nftoken_id[8..48];
            let account_id_bytes = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
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
        let nftoken_id =
            helpers::get_str_field(ctx.tx, "NFTokenID").ok_or(TransactionResult::TemMalformed)?;
        let nft_hash = nft_hash(nftoken_id)?;

        // Remove the token from whichever owner page holds it; drop the page
        // when it becomes empty. The rest of the page (and chain links) is
        // preserved.
        let page_key = nftoken::find_owner_page(ctx.view, &owner_id, &nft_hash)
            .ok_or(TransactionResult::TecNoEntry)?;
        let page_bytes = ctx
            .view
            .read(&page_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut page: Value =
            serde_json::from_slice(&page_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let mut tokens: Vec<Value> = page
            .get("NFTokens")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        tokens.retain(|t| nftoken::entry_nftoken_id(t) != Some(nftoken_id));

        let page_deleted = tokens.is_empty();
        if page_deleted {
            ctx.view
                .erase(&page_key)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            page["NFTokens"] = Value::Array(tokens);
            let page_data =
                serde_json::to_vec(&page).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(page_key, page_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // The issuer's BurnedNFTokens count rises (issuer encoded in the ID).
        if let Some(issuer_id) = issuer_from_id(nftoken_id) {
            let issuer_key = keylet::account(&issuer_id);
            if let Some(b) = ctx.view.read(&issuer_key) {
                let mut iss: Value =
                    serde_json::from_slice(&b).map_err(|_| TransactionResult::TefInternal)?;
                let burned = iss
                    .get("BurnedNFTokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                iss["BurnedNFTokens"] = Value::from(burned + 1);
                let d = serde_json::to_vec(&iss).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(issuer_key, d)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }

        // rippled removes up to 500 offers for the burned NFT, prioritizing
        // sell offers. Directory and owner-account mutations must be included
        // even when the token stays on a nonempty NFTokenPage.
        const MAX_DELETABLE_OFFERS: usize = 500;
        let removed_sells = remove_token_offers_with_limit(
            ctx,
            &keylet::nft_sells(&nft_hash),
            MAX_DELETABLE_OFFERS,
        )?;
        remove_token_offers_with_limit(
            ctx,
            &keylet::nft_buys(&nft_hash),
            MAX_DELETABLE_OFFERS - removed_sells,
        )?;

        // The owner's reserve drops by one only when an entire page was removed.
        if page_deleted {
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

        // The caller consumes its sequence proxy.
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
    use crate::handlers::nftoken_mint::NFTokenMintTransactor;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    #[test]
    fn ledger_105502832_burn_removes_restricted_sell_offer_and_directories() {
        const DESTINATION: &str = "rpx9JThQ2y37FaGeeJP7PXDUVEXY3PHZSC";
        let (mut ledger, nftoken_id) = setup_with_token();
        let owner_id = decode_account_id(OWNER).unwrap();
        let owner_key = keylet::account(&owner_id);
        let mut owner: Value =
            serde_json::from_slice(&LedgerView::new(&ledger).read(&owner_key).unwrap()).unwrap();
        owner["OwnerCount"] = Value::from(2);
        ledger
            .put_state(owner_key, serde_json::to_vec(&owner).unwrap())
            .unwrap();
        let destination_id = decode_account_id(DESTINATION).unwrap();
        let destination_key = keylet::account(&destination_id);
        assert_eq!(
            destination_key.to_string(),
            "587E28972F3B63D2260C0671E59593EE6C4D27AB857D1AF230F5CAA959BB3EAC"
        );
        ledger
            .put_state(
                destination_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": DESTINATION,
                    "Balance": "100000000", "Sequence": 1, "OwnerCount": 0, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        // Keep the NFT page nonempty, as in TX49, so the only reserve change
        // comes from removing the outstanding sell offer.
        let page_key = keylet::nftoken_page_max(&owner_id);
        let mut token_page: Value =
            serde_json::from_slice(&LedgerView::new(&ledger).read(&page_key).unwrap()).unwrap();
        let mut other_id = nftoken_id.clone();
        other_id.replace_range(62..64, "FF");
        token_page["NFTokens"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"NFToken": {"NFTokenID": other_id}}));
        ledger
            .put_state(page_key, serde_json::to_vec(&token_page).unwrap())
            .unwrap();

        let nft_hash = nft_hash(&nftoken_id).unwrap();
        let offer_key = keylet::nftoken_offer(&owner_id, 2);
        let sell_book = keylet::nft_sells(&nft_hash);
        let offer = serde_json::json!({
            "LedgerEntryType": "NFTokenOffer", "Owner": OWNER,
            "NFTokenID": nftoken_id, "Flags": 1, "Amount": "5000000",
            "Destination": DESTINATION, "OwnerNode": "44", "NFTokenOfferNode": "0",
        });
        ledger
            .put_state(offer_key, serde_json::to_vec(&offer).unwrap())
            .unwrap();
        ledger
            .put_state(
                sell_book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode", "RootIndex": sell_book.to_string(),
                    "NFTokenID": nftoken_id, "Flags": 2,
                    "Indexes": [offer_key.to_string()], "IndexNext": "0",
                }))
                .unwrap(),
            )
            .unwrap();
        let owner_dir_page = keylet::dir_node(&keylet::owner_dir(&owner_id), 0x44);
        let unrelated_offer = Hash256::new([0xAA; 32]);
        ledger
            .put_state(
                owner_dir_page,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode", "Owner": OWNER,
                    "RootIndex": keylet::owner_dir(&owner_id).to_string(),
                    "Indexes": [offer_key.to_string(), unrelated_offer.to_string()],
                    "IndexPrevious": "43", "IndexNext": "45", "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenBurn", "Account": OWNER,
            "NFTokenID": nftoken_id, "Fee": "12", "Sequence": 2,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            NFTokenBurnTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );

        assert!(sandbox.read(&offer_key).is_none());
        assert!(
            sandbox.read(&sell_book).is_none(),
            "empty NFT sell book erased"
        );
        let owner_dir: Value =
            serde_json::from_slice(&sandbox.read(&owner_dir_page).unwrap()).unwrap();
        assert_eq!(
            owner_dir["Indexes"],
            serde_json::json!([unrelated_offer.to_string()])
        );
        let owner_after: Value =
            serde_json::from_slice(&sandbox.read(&owner_key).unwrap()).unwrap();
        assert_eq!(owner_after["OwnerCount"], 1);
        assert_eq!(owner_after["BurnedNFTokens"], 1);
        let page_after: Value = serde_json::from_slice(&sandbox.read(&page_key).unwrap()).unwrap();
        assert_eq!(page_after["NFTokens"].as_array().unwrap().len(), 1);
        assert_eq!(
            nftoken::entry_nftoken_id(&page_after["NFTokens"][0]),
            Some(other_id.as_str())
        );
        let changes = sandbox.into_changes();
        assert!(
            changes.updates.contains_key(&destination_key),
            "restricted destination threaded"
        );
    }

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
        let page_key = keylet::nftoken_page_max(&id);
        let page_bytes = sandbox.read(&page_key).unwrap();
        let page: Value = serde_json::from_slice(&page_bytes).unwrap();
        let nftoken_id = page["NFTokens"][0]["NFToken"]["NFTokenID"]
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
        let page_key = keylet::nftoken_page_max(&owner_id);
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
