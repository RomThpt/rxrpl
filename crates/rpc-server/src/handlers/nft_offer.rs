use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{decode_state_value, resolve_ledger};

/// Return information about a specific NFT offer.
///
/// Takes an `nft_offer_id` parameter and returns the details of that
/// particular offer from the ledger state. This is a convenience method
/// for looking up individual NFT offers without needing to enumerate
/// all buy/sell offers for a token.
pub async fn nft_offer(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let offer_id_str = params
        .get("nft_offer_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'nft_offer_id' field".into()))?;

    let offer_id = Hash256::from_str(offer_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_offer_id: {e}")))?;

    let ledger = resolve_ledger(&params, ctx).await?;

    let data = ledger.get_state(&offer_id).ok_or_else(|| {
        RpcServerError::InvalidParams(format!("NFT offer not found: {offer_id_str}"))
    })?;

    let offer: Value = decode_state_value(data)?;

    Ok(serde_json::json!({
        "nft_offer_id": offer_id_str,
        "offer": offer,
        "ledger_current_index": ledger.header.sequence,
    }))
}
