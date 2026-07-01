//! Shared helpers for pseudo-accounts and their empty underlying holdings,
//! used by AMM/Vault/LoanBroker objects that own a hidden account.

use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::ApplyContext;

const ZERO_TXID: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const LSF_LOW_RESERVE: u32 = 0x0001_0000;
const LSF_HIGH_RESERVE: u32 = 0x0002_0000;
const LSF_LOW_NO_RIPPLE: u32 = 0x0010_0000;
const LSF_HIGH_NO_RIPPLE: u32 = 0x0020_0000;

/// Derive a pseudo-account: the first `i` in `0..256` whose account keylet is
/// free, hashing `sha512Half(u16be(i) || parentHash || ownerKey)`.
pub(crate) fn derive_pseudo_account(
    ctx: &ApplyContext<'_>,
    owner_key: &Hash256,
) -> Result<AccountId, TransactionResult> {
    let parent = ctx.view.parent_hash();
    for i in 0u16..256 {
        let ibe = i.to_be_bytes();
        let hash = rxrpl_crypto::sha512_half::sha512_half(&[
            &ibe,
            parent.as_bytes(),
            owner_key.as_bytes(),
        ]);
        let id = rxrpl_codec::address::classic::account_id_from_public_key(hash.as_bytes());
        if !ctx.view.exists(&keylet::account(&id)) {
            return Ok(id);
        }
    }
    Err(TransactionResult::TecDuplicate)
}

/// The (currency-bytes, issuer) of an IOU asset object.
pub(crate) fn asset_currency_issuer(
    asset: &serde_json::Value,
) -> Result<([u8; 20], AccountId), TransactionResult> {
    let currency = asset
        .get("currency")
        .and_then(|c| c.as_str())
        .ok_or(TransactionResult::TemBadCurrency)?;
    let issuer_str = asset
        .get("issuer")
        .and_then(|i| i.as_str())
        .ok_or(TransactionResult::TemBadIssuer)?;
    let issuer_id =
        decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    Ok((helpers::currency_to_bytes(currency), issuer_id))
}

/// Create a pseudo-account's empty trust line to an IOU issuer (rippled's
/// addEmptyHolding for an `Issue`), threading it into both the pseudo and issuer
/// owner directories and restamping the issuer's account. The reserve and
/// NoRipple flags fall on the pseudo side.
pub(crate) fn create_empty_iou_line(
    ctx: &mut ApplyContext<'_>,
    pseudo_id: &AccountId,
    asset: &serde_json::Value,
) -> Result<(), TransactionResult> {
    let (cur_bytes, issuer_id) = asset_currency_issuer(asset)?;
    let issuer_str = encode_account_id(&issuer_id);
    let currency_hex = hex::encode_upper(cur_bytes);

    let tl_key = keylet::trust_line(pseudo_id, &issuer_id, &cur_bytes);
    let pseudo_is_low = pseudo_id.as_bytes() < issuer_id.as_bytes();

    let pseudo_limit = serde_json::json!({
        "currency": currency_hex, "issuer": encode_account_id(pseudo_id), "value": "0",
    });
    let issuer_limit = serde_json::json!({
        "currency": currency_hex, "issuer": issuer_str, "value": "0",
    });
    let (low_limit, high_limit) = if pseudo_is_low {
        (pseudo_limit, issuer_limit)
    } else {
        (issuer_limit, pseudo_limit)
    };
    let flags = if pseudo_is_low {
        LSF_LOW_RESERVE | LSF_LOW_NO_RIPPLE
    } else {
        LSF_HIGH_RESERVE | LSF_HIGH_NO_RIPPLE
    };

    let pseudo_page = crate::owner_dir::add_to_owner_dir(ctx.view, pseudo_id, &tl_key)?;
    let issuer_page = crate::owner_dir::add_to_owner_dir(ctx.view, &issuer_id, &tl_key)?;
    let (low_node, high_node) = if pseudo_is_low {
        (pseudo_page, issuer_page)
    } else {
        (issuer_page, pseudo_page)
    };

    let mut account_one = [0u8; 20];
    account_one[19] = 1;
    let no_account = encode_account_id(&AccountId::from(account_one));
    let tl_obj = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": currency_hex, "issuer": no_account, "value": "0" },
        "LowLimit": low_limit,
        "HighLimit": high_limit,
        "Flags": flags,
        // rippled's trustCreate sets both directory page hints unconditionally
        // (even when 0), so they are always serialized on a created RippleState.
        "LowNode": format!("{low_node:016X}"),
        "HighNode": format!("{high_node:016X}"),
        "PreviousTxnID": ZERO_TXID,
        "PreviousTxnLgrSeq": 0,
    });
    ctx.view
        .insert(
            tl_key,
            serde_json::to_vec(&tl_obj).map_err(|_| TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;

    let issuer_key = keylet::account(&issuer_id);
    if let Some(bytes) = ctx.view.read(&issuer_key) {
        ctx.view
            .update(issuer_key, bytes.to_vec())
            .map_err(|_| TransactionResult::TefInternal)?;
    }

    Ok(())
}

/// True for an `Asset` that is XRP (the string `"XRP"` or `{"currency":"XRP"}`
/// with no issuer).
pub(crate) fn is_xrp_asset(asset: &serde_json::Value) -> bool {
    matches!(asset.as_str(), Some("XRP"))
        || (asset
            .get("currency")
            .and_then(|c| c.as_str())
            .map(|c| c == "XRP")
            .unwrap_or(false)
            && asset.get("issuer").is_none())
}
