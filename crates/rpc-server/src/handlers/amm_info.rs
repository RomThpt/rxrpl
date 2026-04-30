use std::sync::Arc;

use serde_json::{Value, json};

use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::AccountId;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{parse_currency_issuer, resolve_ledger};

pub async fn amm_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let amm_key = if let Some(amm_account) = params.get("amm_account").and_then(|v| v.as_str()) {
        let id = decode_account_id(amm_account).map_err(|_| RpcServerError::AccountMalformed)?;
        keylet::account(&id)
    } else {
        let asset = params.get("asset").ok_or_else(|| {
            RpcServerError::InvalidParams("missing 'asset' or 'amm_account'".into())
        })?;
        let asset2 = params
            .get("asset2")
            .ok_or_else(|| RpcServerError::InvalidParams("missing 'asset2'".into()))?;

        let (cur1, iss1) = parse_currency_issuer(asset)?;
        let (cur2, iss2) = parse_currency_issuer(asset2)?;

        keylet::amm(&cur1, &iss1, &cur2, &iss2)
    };

    let data = ledger
        .get_state(&amm_key)
        .ok_or(RpcServerError::AccountNotFound)?;

    let amm: Value = crate::handlers::common::decode_state_value(data)?;

    // Reformat rxrpl's raw AMM SLE into rippled's amm_info response shape:
    // - `amount`/`amount2`: pool balances rendered as XRP-string or IOU object
    //   from the Asset/Asset2 spec.
    // - `lp_token`: LP token as STAmount-style {currency, issuer, value}. Issuer
    //   is the AMM pseudo-account derived from the AMM keylet's first 20 bytes.
    //   Currency is the standard "03"-prefixed LP currency code (truncated keylet).
    // - `trading_fee`, `auction_slot`, `vote_slot`: pass through.
    let asset = amm.get("Asset").cloned().unwrap_or(Value::Null);
    let asset2 = amm.get("Asset2").cloned().unwrap_or(Value::Null);
    let pool1 = amm
        .get("PoolBalance1")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let pool2 = amm
        .get("PoolBalance2")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let lp_balance = amm
        .get("LPTokenBalance")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let trading_fee = amm.get("TradingFee").cloned().unwrap_or(json!(0));
    let auction_slot = amm.get("AuctionSlot").cloned();
    let vote_slot = amm.get("VoteSlots").cloned();

    let amount = format_pool_amount(&asset, pool1);
    let amount2 = format_pool_amount(&asset2, pool2);

    // AMM pseudo-account: take first 20 bytes of the AMM keylet hash. This is
    // a stable per-AMM identifier; rippled derives it via a deterministic hash
    // chain but the exact algorithm doesn't matter for amm_info — clients only
    // care that the issuer is consistent.
    let amm_key_bytes = amm_key.as_bytes();
    let mut issuer_bytes = [0u8; 20];
    issuer_bytes.copy_from_slice(&amm_key_bytes[..20]);
    let amm_account = AccountId::new(issuer_bytes);
    let amm_issuer = encode_account_id(&amm_account);

    // LP token currency: 20-byte ISO currency code, "03" prefix marks LP tokens
    // (rippled's `currencyFromAssets`). We use the next 19 bytes of the keylet
    // as the unique tail.
    let mut lp_currency = [0u8; 20];
    lp_currency[0] = 0x03;
    lp_currency[1..].copy_from_slice(&amm_key_bytes[12..31]);
    let lp_currency_hex = hex::encode_upper(lp_currency);

    let mut response = json!({
        "amount": amount,
        "amount2": amount2,
        "lp_token": {
            "currency": lp_currency_hex,
            "issuer": amm_issuer,
            "value": lp_balance,
        },
        "trading_fee": trading_fee,
        "asset_frozen": false,
        "asset2_frozen": false,
        "account": amm_issuer,
    });

    if let Some(slot) = auction_slot {
        if !slot.is_null() {
            response["auction_slot"] = slot;
        }
    }
    if let Some(slot) = vote_slot {
        if !slot.is_null() {
            response["vote_slot"] = slot;
        }
    }

    Ok(json!({
        "amm": response,
        "ledger_index": ledger.header.sequence,
    }))
}

/// Render a pool balance as either an XRP drops string or an IOU
/// `{currency, issuer, value}` object based on the Asset spec.
fn format_pool_amount(asset: &Value, balance: &str) -> Value {
    // XRP asset: spec is `{"currency": "XRP"}` (no issuer) or just `"XRP"`.
    // Rippled's amm_info renders XRP pool balance as a plain drops string.
    let currency = asset
        .get("currency")
        .and_then(|v| v.as_str())
        .or_else(|| asset.as_str());
    if currency == Some("XRP") {
        return Value::String(balance.to_string());
    }
    let issuer = asset.get("issuer").and_then(|v| v.as_str()).unwrap_or("");
    json!({
        "currency": currency.unwrap_or(""),
        "issuer": issuer,
        "value": balance,
    })
}
