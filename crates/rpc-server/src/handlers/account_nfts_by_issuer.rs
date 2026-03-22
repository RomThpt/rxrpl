use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger};

/// Get NFTs owned by an account, filtered by issuer.
///
/// Works like `account_nfts` but only returns NFTs minted by
/// the specified issuer account.
pub async fn account_nfts_by_issuer(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    let issuer_str = params
        .get("issuer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'issuer' field".into()))?;

    let issuer_id = rxrpl_codec::address::classic::decode_account_id(issuer_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid issuer: {e}")))?;

    let issuer_bytes = issuer_id.0;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as usize;

    let page_key = keylet::nftoken_page_min(&account_id);

    let mut nfts = Vec::new();

    let mut current_key = page_key;
    while let Some(data) = ledger.get_state(&current_key) {
        let page: Value = crate::handlers::common::decode_state_value(data)?;

        if let Some(tokens) = page.get("NFTokens").and_then(|v| v.as_array()) {
            for token in tokens {
                if nfts.len() >= limit {
                    break;
                }
                let nft = token.get("NFToken").unwrap_or(token);

                // Filter by issuer: extract issuer from NFTokenID bytes 4..24
                if let Some(id_str) = nft.get("NFTokenID").and_then(|v| v.as_str()) {
                    if let Ok(id_bytes) = hex::decode(id_str) {
                        if id_bytes.len() == 32 && id_bytes[4..24] == issuer_bytes {
                            nfts.push(nft.clone());
                        }
                    }
                }
            }
        }

        if nfts.len() >= limit {
            break;
        }

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
        "issuer": issuer_str,
        "account_nfts": nfts,
        "ledger_index": ledger.header.sequence,
    }))
}
