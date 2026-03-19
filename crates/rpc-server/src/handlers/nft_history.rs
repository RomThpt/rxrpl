use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use rxrpl_primitives::Hash256;

/// Look up the transaction history for a specific NFT.
///
/// Returns transactions that affected the given NFT, including
/// minting, transfers, burns, and offer operations.
///
/// This requires scanning transaction history, which is not yet
/// fully indexed by NFT ID. Returns an empty list until NFT
/// transaction indexing is implemented.
pub async fn nft_history(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let nft_id_str = params
        .get("nft_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'nft_id'".into()))?;

    let _nft_id = Hash256::from_str(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id: {e}")))?;

    let ledger_index_min = params
        .get("ledger_index_min")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let ledger_index_max = params
        .get("ledger_index_max")
        .and_then(|v| v.as_u64())
        .unwrap_or(u32::MAX as u64) as u32;

    // NFT transaction indexing is not yet implemented.
    // When available, this will query the transaction store filtered by NFT ID.
    let transactions: Vec<Value> = Vec::new();

    Ok(serde_json::json!({
        "nft_id": nft_id_str,
        "ledger_index_min": ledger_index_min,
        "ledger_index_max": ledger_index_max,
        "transactions": transactions,
        "validated": true,
    }))
}
