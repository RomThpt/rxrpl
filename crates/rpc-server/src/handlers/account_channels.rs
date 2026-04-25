use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

pub async fn account_channels(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    if ledger.get_state(&keylet::account(&account_id)).is_none() {
        return Err(RpcServerError::AccountNotFound);
    }

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as usize;
    let marker = params.get("marker").and_then(|v| v.as_str());

    let (entries, next_marker) = walk_owner_directory(&ledger, &account_id, limit, marker)?;

    let channels: Vec<Value> = entries
        .into_iter()
        .filter(|(_, entry)| {
            entry.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("PayChannel")
        })
        .map(|(idx, entry)| {
            serde_json::json!({
                "channel_id": idx.to_string(),
                "account": entry.get("Account").unwrap_or(&Value::Null),
                "destination": entry.get("Destination").unwrap_or(&Value::Null),
                "amount": entry.get("Amount").unwrap_or(&Value::Null),
                "balance": entry.get("Balance").unwrap_or(&Value::Null),
                "settle_delay": entry.get("SettleDelay").unwrap_or(&Value::Null),
                "public_key": entry.get("PublicKey").unwrap_or(&Value::Null),
            })
        })
        .collect();

    let account_str = params["account"].as_str().unwrap_or_default();
    let mut result = serde_json::json!({
        "account": account_str,
        "channels": channels,
        "ledger_index": ledger.header.sequence,
    });

    if let Some(m) = next_marker {
        result["marker"] = Value::String(m);
    }

    Ok(result)
}
