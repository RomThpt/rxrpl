use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{parse_currency_issuer, resolve_ledger};

pub async fn amm_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let amm_key = if let Some(amm_account) = params.get("amm_account").and_then(|v| v.as_str()) {
        // Direct lookup by AMM account
        let id = decode_account_id(amm_account)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid amm_account: {e}")))?;
        keylet::account(&id)
    } else {
        // Lookup by asset pair
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
        .ok_or_else(|| RpcServerError::InvalidParams("AMM not found".into()))?;

    let amm: Value = serde_json::from_slice(data)
        .map_err(|e| RpcServerError::Internal(format!("failed to deserialize AMM: {e}")))?;

    Ok(serde_json::json!({
        "amm": amm,
        "ledger_index": ledger.header.sequence,
    }))
}
