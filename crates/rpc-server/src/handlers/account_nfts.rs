use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger};

pub async fn account_nfts(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as usize;

    // NFToken pages use a linked list, not the owner directory.
    // Start from the minimum page for this account.
    let page_key = keylet::nftoken_page_min(&account_id);

    let mut nfts = Vec::new();

    let mut current_key = page_key;
    while let Some(data) = ledger.get_state(&current_key) {
        let page: Value = serde_json::from_slice(data).map_err(|e| {
            RpcServerError::Internal(format!("failed to deserialize nft page: {e}"))
        })?;

        if let Some(tokens) = page.get("NFTokens").and_then(|v| v.as_array()) {
            for token in tokens {
                if nfts.len() >= limit {
                    break;
                }
                if let Some(nft) = token.get("NFToken") {
                    nfts.push(nft.clone());
                } else {
                    nfts.push(token.clone());
                }
            }
        }

        if nfts.len() >= limit {
            break;
        }

        // Follow linked list via NextPageMin
        match page.get("NextPageMin").and_then(|v| v.as_str()) {
            Some(next_str) => {
                current_key = next_str
                    .parse()
                    .map_err(|e| RpcServerError::Internal(format!("invalid next page key: {e}")))?;
            }
            None => break,
        }
    }

    let account_str = params["account"].as_str().unwrap_or_default();
    Ok(serde_json::json!({
        "account": account_str,
        "account_nfts": nfts,
        "ledger_index": ledger.header.sequence,
    }))
}
