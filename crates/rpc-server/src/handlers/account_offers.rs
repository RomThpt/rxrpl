use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

pub async fn account_offers(
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
    let marker = params.get("marker").and_then(|v| v.as_str());

    let (entries, next_marker) = walk_owner_directory(&ledger, &account_id, limit, marker)?;

    let offers: Vec<Value> = entries
        .into_iter()
        .filter(|(_, entry)| entry.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("Offer"))
        .map(|(_, entry)| {
            serde_json::json!({
                "seq": entry.get("Sequence").unwrap_or(&Value::Null),
                "flags": entry.get("Flags").unwrap_or(&Value::Null),
                "taker_pays": entry.get("TakerPays").unwrap_or(&Value::Null),
                "taker_gets": entry.get("TakerGets").unwrap_or(&Value::Null),
                "quality": entry.get("BookDirectory").unwrap_or(&Value::Null),
            })
        })
        .collect();

    let account_str = params["account"].as_str().unwrap_or_default();
    let mut result = serde_json::json!({
        "account": account_str,
        "offers": offers,
        "ledger_index": ledger.header.sequence,
    });

    if let Some(m) = next_marker {
        result["marker"] = Value::String(m);
    }

    Ok(result)
}
