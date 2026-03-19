use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

pub async fn account_lines(
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

    let peer_filter = if let Some(peer_str) = params.get("peer").and_then(|v| v.as_str()) {
        Some(
            decode_account_id(peer_str)
                .map_err(|e| RpcServerError::InvalidParams(format!("invalid peer: {e}")))?,
        )
    } else {
        None
    };

    let (entries, next_marker) = walk_owner_directory(&ledger, &account_id, limit, marker)?;

    let mut lines = Vec::new();

    for (_, entry) in entries {
        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("RippleState") {
            continue;
        }

        let high_limit = entry.get("HighLimit").unwrap_or(&Value::Null);
        let low_limit = entry.get("LowLimit").unwrap_or(&Value::Null);

        let high_account = high_limit
            .get("issuer")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let low_account = low_limit
            .get("issuer")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let account_str = params["account"].as_str().unwrap_or_default();
        let is_high = high_account == account_str;
        let peer_account = if is_high { low_account } else { high_account };

        // Apply peer filter
        if let Some(ref peer) = peer_filter {
            let peer_id = decode_account_id(peer_account).ok();
            if peer_id.as_ref() != Some(peer) {
                continue;
            }
        }

        let balance_str = entry
            .get("Balance")
            .and_then(|v| {
                v.get("value")
                    .and_then(|v| v.as_str())
                    .or_else(|| v.as_str())
            })
            .unwrap_or("0");

        // Negate balance if we're the high account
        let balance = if is_high {
            if let Some(stripped) = balance_str.strip_prefix('-') {
                stripped.to_string()
            } else if balance_str == "0" {
                "0".to_string()
            } else {
                format!("-{balance_str}")
            }
        } else {
            balance_str.to_string()
        };

        let currency = entry
            .get("Balance")
            .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            .or_else(|| {
                entry
                    .get("HighLimit")
                    .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            })
            .unwrap_or("???");

        let limit_val = if is_high {
            high_limit
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
        } else {
            low_limit
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
        };
        let limit_peer = if is_high {
            low_limit
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
        } else {
            high_limit
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
        };

        lines.push(serde_json::json!({
            "account": peer_account,
            "balance": balance,
            "currency": currency,
            "limit": limit_val,
            "limit_peer": limit_peer,
        }));
    }

    let account_str = params["account"].as_str().unwrap_or_default();
    let mut result = serde_json::json!({
        "account": account_str,
        "lines": lines,
        "ledger_index": ledger.header.sequence,
    });

    if let Some(m) = next_marker {
        result["marker"] = Value::String(m);
    }

    Ok(result)
}
