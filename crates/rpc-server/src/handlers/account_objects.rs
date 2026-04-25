use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

/// Map snake_case `type` aliases to the PascalCase `LedgerEntryType`
/// values that ledger entries actually carry. Unknown / already-PascalCase
/// values pass through unchanged so callers using either convention work.
fn map_type_alias(t: &str) -> &str {
    match t {
        "check" => "Check",
        "deposit_preauth" => "DepositPreauth",
        "escrow" => "Escrow",
        "nft_offer" => "NFTokenOffer",
        "offer" => "Offer",
        "payment_channel" => "PayChannel",
        "signer_list" => "SignerList",
        "state" => "RippleState",
        "ticket" => "Ticket",
        "amm" => "AMM",
        "did" => "DID",
        "oracle" => "Oracle",
        "hook" => "Hook",
        "hook_state" => "HookState",
        "hook_definition" => "HookDefinition",
        "mptoken_issuance" => "MPTokenIssuance",
        "mptoken" => "MPToken",
        "vault" => "Vault",
        "credential" => "Credential",
        other => other,
    }
}

pub async fn account_objects(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    let type_filter = params
        .get("type")
        .and_then(|v| v.as_str())
        .map(map_type_alias);
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as usize;
    let marker = params.get("marker").and_then(|v| v.as_str());

    let (entries, next_marker) = walk_owner_directory(&ledger, &account_id, limit, marker)?;

    let objects: Vec<Value> = entries
        .into_iter()
        .filter(|(_, entry)| {
            if let Some(filter) = type_filter {
                entry
                    .get("LedgerEntryType")
                    .and_then(|v| v.as_str())
                    .map(|t| t == filter)
                    .unwrap_or(false)
            } else {
                true
            }
        })
        .map(|(idx, entry)| {
            // rippled embeds the entry's ledger index inside each object so
            // callers can use it as a unique identifier (e.g. CheckCash needs
            // CheckID = the Check entry's index).
            let mut obj = entry.as_object().cloned().unwrap_or_default();
            obj.entry("index".to_string())
                .or_insert_with(|| Value::String(idx.to_string()));
            Value::Object(obj)
        })
        .collect();

    let account_str = params["account"].as_str().unwrap_or_default();
    let mut result = serde_json::json!({
        "account": account_str,
        "account_objects": objects,
        "ledger_index": ledger.header.sequence,
    });

    if let Some(m) = next_marker {
        result["marker"] = Value::String(m);
    }

    Ok(result)
}
