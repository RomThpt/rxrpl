use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::owner_dir::{add_to_nft_offer_dir, add_to_owner_dir};
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// tfSellNFToken flag
const TF_SELL_NFTOKEN: u32 = 0x0001;

/// True when an offer amount is zero (XRP `"0"` drops or IOU value `0`).
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

fn nft_hash(id_hex: &str) -> Result<Hash256, TransactionResult> {
    let bytes = hex::decode(id_hex).map_err(|_| TransactionResult::TemMalformed)?;
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

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

        // tfOnlyXRP is encoded in the NFTokenID's 16-bit flags field (the first
        // 4 hex chars). When set, any offer Amount MUST be XRP — IOU offers are
        // rejected with temBAD_AMOUNT (mirrors rippled checkAmount).
        const NFT_FLAG_ONLY_XRP: u32 = 0x0002;
        let nft_flags = u32::from_str_radix(&id[..4], 16).unwrap_or(0);
        if nft_flags & NFT_FLAG_ONLY_XRP != 0 {
            // If Amount is an IOU object (not a string), reject.
            if let Some(amt) = ctx.tx.get("Amount") {
                if amt.is_object() {
                    return Err(TransactionResult::TemBadAmount);
                }
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // The sender account must exist. The NFToken ownership check
        // (tecNO_ENTRY) and the owner-reserve check (tecINSUFFICIENT_RESERVE)
        // are CLAIMED tecs — they must charge the fee and sequence — so they run
        // in `apply`, which routes through the engine's central fee/sequence
        // consume. A tec returned from preclaim short-circuits before that
        // consume and would wrongly charge nothing.
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

        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let is_sell = flags & TF_SELL_NFTOKEN != 0;
        let nftoken_id = helpers::get_str_field(ctx.tx, "NFTokenID")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();

        // NFToken ownership (rippled NFTokenCreateOffer::preclaim findToken): the
        // token must exist in its holder's page chain — the seller (sfAccount)
        // for a sell offer, the named sfOwner for a buy offer — else tecNO_ENTRY.
        // A buy offer's sfOwner is required. This is a CLAIMED tec (fee and
        // sequence charged, no offer); it runs in apply so the engine's central
        // fee/sequence consume applies, and precedes the reserve check exactly as
        // preclaim precedes doApply in rippled.
        let token_owner = if is_sell {
            account_id
        } else {
            let owner_str =
                helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
            decode_account_id(owner_str).map_err(|_| TransactionResult::TemMalformed)?
        };
        let owns = nft_hash(&nftoken_id)
            .ok()
            .and_then(|h| crate::nftoken::find_owner_page(ctx.view, &token_owner, &h))
            .and_then(|pk| ctx.view.read(&pk))
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|page| page.get("NFTokens").cloned())
            .and_then(|tokens| tokens.as_array().cloned())
            .map(|toks| {
                toks.iter()
                    .any(|t| crate::nftoken::entry_nftoken_id(t) == Some(nftoken_id.as_str()))
            })
            .unwrap_or(false);
        if !owns {
            return Err(TransactionResult::TecNoEntry);
        }

        // DisallowIncomingNFTokenOffer (rippled nft::tokenOfferCreatePreclaim):
        // the offer is refused with tecNO_PERMISSION when a named Destination, or
        // (for a buy offer) the token's Owner, has set lsfDisallowIncomingNFTokenOffer
        // on its AccountRoot. rippled checks the destination first, then the owner,
        // both in preclaim — i.e. before the reserve test in doApply — so this runs
        // after the tecNO_ENTRY ownership check and before the reserve check below.
        // Like those, it is a CLAIMED tec (fee/sequence charged, no offer created).
        const LSF_DISALLOW_INCOMING_NFTOKEN_OFFER: u32 = 0x0400_0000;
        if let Some(dest) = helpers::get_str_field(ctx.tx, "Destination") {
            if let Ok((_, dst_acct)) = helpers::read_account_by_address(ctx.view, dest) {
                if helpers::get_flags(&dst_acct) & LSF_DISALLOW_INCOMING_NFTOKEN_OFFER != 0 {
                    return Err(TransactionResult::TecNoPermission);
                }
            }
        }
        if !is_sell {
            // Buy offer: `token_owner` is the named sfOwner whose AccountRoot
            // must permit incoming NFToken offers.
            if let Some(owner_acct) = ctx
                .view
                .read(&keylet::account(&token_owner))
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            {
                if helpers::get_flags(&owner_acct) & LSF_DISALLOW_INCOMING_NFTOKEN_OFFER != 0 {
                    return Err(TransactionResult::TecNoPermission);
                }
            }
        }

        // Owner reserve (rippled nft::tokenOfferCreateApply): a new NFTokenOffer
        // adds 1 owned object, so the creator must fund the reserve for one more
        // entry. rippled compares its `mPriorBalance` (the XRP balance *before*
        // the fee) against `accountReserve(OwnerCount + 1)` and returns
        // tecINSUFFICIENT_RESERVE — fee and sequence charged, no offer — when it
        // falls short. The engine consumed the fee centrally before doApply, so
        // reconstruct mPriorBalance by adding it back.
        let owner_count = helpers::get_owner_count(&acct);
        let prior_balance = helpers::get_balance(&acct).saturating_add(helpers::get_fee(ctx.tx));

        // rippled nft::tokenOfferCreatePreclaim: a buy offer requires the
        // account to hold positive funds of the offered Amount at hand
        // (`accountFunds(...).signum() <= 0` -> tecUNFUNDED_OFFER). For XRP that
        // is spendable balance = prior balance minus the current owner reserve.
        // This claimed tec precedes the owner-reserve test below, matching the
        // preclaim/doApply ordering in rippled.
        if !is_sell
            && ctx.tx.get("Amount").map(Value::is_string).unwrap_or(false)
            && prior_balance.saturating_sub(ctx.fees.account_reserve(owner_count)) == 0
        {
            return Err(TransactionResult::TecUnfundedOffer);
        }

        if prior_balance < ctx.fees.account_reserve(owner_count + 1) {
            return Err(TransactionResult::TecInsufficientReserve);
        }

        // The NFTokenOffer's keylet/Sequence is the TX seq-proxy value (the
        // engine already consumed the sender's Sequence/Ticket centrally).
        let tx_seq = helpers::tx_seq_proxy_value(ctx.tx);
        helpers::adjust_owner_count(&mut acct, 1);

        let acct_data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create NFTokenOffer entry
        let offer_key = keylet::nftoken_offer(&account_id, tx_seq);

        // Link into the creator's owner directory and the per-NFToken offer
        // book (buy or sell), recording the owner-directory page as sfOwnerNode
        // and the per-NFToken offer-directory page as sfNFTokenOfferNode. Both
        // are SoeRequired on ltNFTOKEN_OFFER (ledger_entries.macro), so rippled
        // always serializes them (0 for the root page).
        let owner_node = add_to_owner_dir(ctx.view, &account_id, &offer_key)?;
        let nft_bytes: [u8; 32] = hex::decode(&nftoken_id)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(TransactionResult::TemMalformed)?;
        let nft_id_hash = Hash256::new(nft_bytes);
        let book_key = if is_sell {
            keylet::nft_sells(&nft_id_hash)
        } else {
            keylet::nft_buys(&nft_id_hash)
        };
        let offer_node =
            add_to_nft_offer_dir(ctx.view, &book_key, &nftoken_id, &offer_key, is_sell)?;

        // Amount passes through in its original shape (XRP drops string or IOU
        // object). rippled stores no Sequence on the offer, and sfFlags only
        // when non-zero (a sell offer carries tfSellNFToken).
        let amount_value = ctx
            .tx
            .get("Amount")
            .cloned()
            .unwrap_or_else(|| Value::String("0".to_string()));
        let mut offer = serde_json::json!({
            "LedgerEntryType": "NFTokenOffer",
            // rippled sets sfFlags on the NFTokenOffer unconditionally
            // (tokenOfferCreateApply), so it serialises even for a buy offer
            // (Flags 0). It is NOT default-droppable here — omitting it diverges
            // the account_hash (the metadata NewFields hides the zero, so the
            // per-tx value check cannot see it).
            "Flags": flags,
            "Owner": account_str,
            "NFTokenID": nftoken_id,
            "OwnerNode": format!("{owner_node:016X}"),
            "NFTokenOfferNode": format!("{offer_node:016X}"),
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        // sfAmount is default-droppable: a zero-amount (gift) sell offer omits
        // it, matching rippled's serialization.
        if !amount_is_zero(&amount_value) {
            offer["Amount"] = amount_value;
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

        // A Destination-restricted offer touches the destination AccountRoot
        // (its PreviousTxnID is threaded though no field changes), as rippled
        // does when validating the named destination.
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
    const OWNER: &str = "rLNaPoKeeBjZe2qs6x52yVPZpZ8td4dc6w";
    const NFTOKEN_ID: &str = "00000000000000000000000000000000B5F762798A53D543A014CAF8B297CFF8";

    fn put_account(ledger: &mut Ledger, account: &str, balance: &str) {
        let id = decode_account_id(account).unwrap();
        let key = keylet::account(&id);
        let obj = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": account,
            "Balance": balance,
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&obj).unwrap())
            .unwrap();
    }

    fn setup_ledger() -> Ledger {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ACCOUNT, "100000000");
        ledger
    }

    /// Seed `holder`'s NFTokenPage chain so it owns NFTOKEN_ID, mirroring the
    /// on-chain state that NFTokenCreateOffer::preclaim `findToken` requires.
    fn seed_nft(sandbox: &mut Sandbox, holder: &str) {
        let id = decode_account_id(holder).unwrap();
        let nft: Hash256 = NFTOKEN_ID.parse().unwrap();
        let entry = serde_json::json!({ "NFToken": { "NFTokenID": NFTOKEN_ID } });
        crate::nftoken::insert_token(sandbox, &id, &nft, entry).unwrap();
    }

    #[test]
    fn create_sell_offer() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        seed_nft(&mut sandbox, ACCOUNT);
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
        // sfOwnerNode and sfNFTokenOfferNode are SoeRequired on ltNFTOKEN_OFFER,
        // so both are always serialized (0 for the root directory page).
        assert_eq!(offer["OwnerNode"].as_str().unwrap(), "0000000000000000");
        assert_eq!(
            offer["NFTokenOfferNode"].as_str().unwrap(),
            "0000000000000000"
        );
    }

    #[test]
    fn create_buy_offer() {
        let mut ledger = setup_ledger();
        put_account(&mut ledger, OWNER, "100000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // The named Owner must hold the NFT for the buy offer to be placed.
        seed_nft(&mut sandbox, OWNER);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "Owner": OWNER,
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
    fn buy_offer_unknown_owner_token_is_no_entry() {
        let mut ledger = setup_ledger();
        put_account(&mut ledger, OWNER, "100000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // Owner does NOT hold the NFT — no page seeded.
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "Owner": OWNER,
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
        assert_eq!(
            NFTokenCreateOfferTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn buy_offer_owner_disallows_incoming_is_no_permission() {
        let mut ledger = setup_ledger();
        // Owner exists and holds the NFT, but has set
        // lsfDisallowIncomingNFTokenOffer (0x04000000) on its AccountRoot.
        let owner_id = decode_account_id(OWNER).unwrap();
        let owner_obj = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0x0400_0000u32,
        });
        ledger
            .put_state(
                keylet::account(&owner_id),
                serde_json::to_vec(&owner_obj).unwrap(),
            )
            .unwrap();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        seed_nft(&mut sandbox, OWNER);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "NFTokenCreateOffer",
            "Account": ACCOUNT,
            "Owner": OWNER,
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
        assert_eq!(
            NFTokenCreateOfferTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn sell_offer_below_reserve_is_insufficient_reserve() {
        let mut ledger = Ledger::genesis();
        // Balance under the single-object reserve (account_reserve(1)).
        put_account(&mut ledger, ACCOUNT, "11000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        seed_nft(&mut sandbox, ACCOUNT);
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
        assert_eq!(
            NFTokenCreateOfferTransactor.apply(&mut ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
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
