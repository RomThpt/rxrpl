use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;
use rxrpl_primitives::Hash256;

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

    // NFT buy offers are stored in a directory keyed by the NFT ID with a buy-specific namespace.
    // The directory root is computed from the NFT ID bytes.
    // For simplicity, we walk the directory if it exists.
    let mut buy_dir_bytes = [0u8; 32];
    let nft_bytes = hex::decode(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id hex: {e}")))?;
    if nft_bytes.len() == 32 {
        buy_dir_bytes.copy_from_slice(&nft_bytes);
    }

    // Use a computed key for the buy offer directory
    let dir_key = Hash256::from(buy_dir_bytes);

    let mut offers = Vec::new();

    if let Some(data) = ledger.get_state(&dir_key) {
        let dir: Value = crate::handlers::common::decode_state_value(data)?;

        if let Some(indexes) = dir.get("Indexes").and_then(|v| v.as_array()) {
            for idx_val in indexes {
                if offers.len() >= limit {
                    break;
                }
                let idx_str = idx_val.as_str().unwrap_or_default();
                let idx_hash: Hash256 = idx_str
                    .parse()
                    .map_err(|e| RpcServerError::Internal(format!("invalid index: {e}")))?;

                if let Some(entry_data) = ledger.get_state(&idx_hash) {
                    let entry: Value = crate::handlers::common::decode_state_value(entry_data)?;
                    offers.push(entry);
                }
            }
        }
    }

    Ok(serde_json::json!({
        "nft_id": nft_id_str,
        "offers": offers,
        "ledger_index": ledger.header.sequence,
    }))
}
