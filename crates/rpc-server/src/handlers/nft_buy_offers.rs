use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// `tfSellNFToken` flag — set on the offer when it's a sell offer.
const TF_SELL_NFTOKEN: u64 = 0x0001;

pub async fn nft_buy_offers(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let nft_id_str = params
        .get("nft_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'nft_id'".into()))?;

    let _nft_id = Hash256::from_str(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id: {e}")))?;

    let ledger = resolve_ledger(&params, ctx).await?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(250)
        .min(500) as usize;

    // We don't yet maintain per-NFT sell/buy offer directories; scan the
    // entire state map for `NFTokenOffer` entries matching this NFT id and
    // not the sell flag.
    let mut offers = Vec::new();
    for (idx, raw) in ledger.state_map.iter_ref() {
        if offers.len() >= limit {
            break;
        }
        let entry: Value = match crate::handlers::common::decode_state_value(raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("NFTokenOffer") {
            continue;
        }
        if entry.get("NFTokenID").and_then(|v| v.as_str()) != Some(nft_id_str) {
            continue;
        }
        let flags = entry.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        if flags & TF_SELL_NFTOKEN != 0 {
            continue;
        }
        let mut obj = entry.as_object().cloned().unwrap_or_default();
        obj.entry("nft_offer_index".to_string())
            .or_insert_with(|| Value::String(idx.to_string()));
        offers.push(Value::Object(obj));
    }

    if offers.is_empty() {
        return Err(RpcServerError::ObjectNotFound);
    }

    Ok(serde_json::json!({
        "nft_id": nft_id_str,
        "offers": offers,
        "ledger_index": ledger.header.sequence,
    }))
}
