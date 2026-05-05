use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

pub async fn account_currencies(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    // Walk the entire directory to collect all currencies
    let (entries, _) = walk_owner_directory(&ledger, &account_id, usize::MAX, None)?;

    let mut send_currencies = BTreeSet::new();
    let mut receive_currencies = BTreeSet::new();

    let account_str = params["account"].as_str().unwrap_or_default();

    for (_, entry) in entries {
        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("RippleState") {
            continue;
        }

        let currency = entry
            .get("Balance")
            .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            .or_else(|| {
                entry
                    .get("HighLimit")
                    .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            });

        let Some(currency) = currency else { continue };

        let high_account = entry
            .get("HighLimit")
            .and_then(|v| v.get("issuer").and_then(|i| i.as_str()))
            .unwrap_or_default();
        let _low_account = entry
            .get("LowLimit")
            .and_then(|v| v.get("issuer").and_then(|i| i.as_str()))
            .unwrap_or_default();

        let is_high = high_account == account_str;

        let my_limit = if is_high {
            entry
                .get("HighLimit")
                .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                .unwrap_or("0")
        } else {
            entry
                .get("LowLimit")
                .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                .unwrap_or("0")
        };

        let peer_limit = if is_high {
            entry
                .get("LowLimit")
                .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                .unwrap_or("0")
        } else {
            entry
                .get("HighLimit")
                .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                .unwrap_or("0")
        };

        // Per rippled: account can receive a currency when it trusts the
        // issuer for at least 1 unit (its own limit on the line is non-zero).
        // It can send a currency when it currently holds a positive balance
        // or has a non-zero peer limit allowing it to go negative.
        if my_limit != "0" {
            receive_currencies.insert(currency.to_string());
        }

        let balance_str = entry
            .get("Balance")
            .and_then(|v| v.get("value").and_then(|v| v.as_str()))
            .unwrap_or("0");
        let balance: f64 = balance_str.parse().unwrap_or(0.0);
        let signed_balance = if is_high { -balance } else { balance };
        if signed_balance > 0.0 || peer_limit != "0" {
            send_currencies.insert(currency.to_string());
        }
    }

    Ok(serde_json::json!({
        "send_currencies": send_currencies.into_iter().collect::<Vec<_>>(),
        "receive_currencies": receive_currencies.into_iter().collect::<Vec<_>>(),
        "ledger_index": ledger.header.sequence,
    }))
}
