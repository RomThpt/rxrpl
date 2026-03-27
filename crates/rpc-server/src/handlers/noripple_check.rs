use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

/// Check for problematic NoRipple flag settings on trust lines.
///
/// For a gateway, outgoing trust lines should have NoRipple disabled.
/// For a user, trust lines should have NoRipple enabled.
pub async fn noripple_check(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    let role = params.get("role").and_then(|v| v.as_str()).ok_or_else(|| {
        RpcServerError::InvalidParams("missing 'role' (must be 'gateway' or 'user')".into())
    })?;

    if role != "gateway" && role != "user" {
        return Err(RpcServerError::InvalidParams(
            "role must be 'gateway' or 'user'".into(),
        ));
    }

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(300)
        .min(400) as usize;

    let include_transactions = params
        .get("transactions")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let account_str = params["account"].as_str().unwrap_or_default();

    // Check default ripple flag on account root
    let account_key = keylet::account(&account_id);
    let mut problems: Vec<Value> = Vec::new();
    let mut transactions: Vec<Value> = Vec::new();

    if let Some(data) = ledger.get_state(&account_key) {
        let account_root: Value = crate::handlers::common::decode_state_value(data)?;

        let flags = account_root
            .get("Flags")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let has_default_ripple = flags & 0x00800000 != 0; // lsfDefaultRipple

        if role == "gateway" && !has_default_ripple {
            problems.push(Value::String(
                "You should immediately set your default ripple flag".into(),
            ));
            if include_transactions {
                transactions.push(serde_json::json!({
                    "TransactionType": "AccountSet",
                    "Account": account_str,
                    "SetFlag": 8, // asfDefaultRipple
                }));
            }
        } else if role == "user" && has_default_ripple {
            problems.push(Value::String(
                "You should clear your default ripple flag".into(),
            ));
            if include_transactions {
                transactions.push(serde_json::json!({
                    "TransactionType": "AccountSet",
                    "Account": account_str,
                    "ClearFlag": 8, // asfDefaultRipple
                }));
            }
        }
    }

    // Walk trust lines
    let (entries, _) = walk_owner_directory(&ledger, &account_id, limit, None)?;

    for (_, entry) in &entries {
        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("RippleState") {
            continue;
        }

        let flags = entry.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);

        let high_account = entry
            .get("HighLimit")
            .and_then(|v| v.get("issuer").and_then(|v| v.as_str()))
            .unwrap_or_default();

        let is_high = high_account == account_str;

        // NoRipple flags: lsfHighNoRipple = 0x00200000, lsfLowNoRipple = 0x00100000
        let has_no_ripple = if is_high {
            flags & 0x00200000 != 0
        } else {
            flags & 0x00100000 != 0
        };

        let peer_account = if is_high {
            entry
                .get("LowLimit")
                .and_then(|v| v.get("issuer").and_then(|v| v.as_str()))
                .unwrap_or_default()
        } else {
            high_account
        };

        let currency = entry
            .get("Balance")
            .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            .unwrap_or("???");

        if role == "gateway" && has_no_ripple {
            problems.push(Value::String(format!(
                "You should clear the no ripple flag on your {currency} line to {peer_account}"
            )));
            if include_transactions {
                transactions.push(serde_json::json!({
                    "TransactionType": "TrustSet",
                    "Account": account_str,
                    "LimitAmount": {
                        "currency": currency,
                        "issuer": peer_account,
                        "value": entry.get(if is_high { "HighLimit" } else { "LowLimit" })
                            .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                            .unwrap_or("0"),
                    },
                    "Flags": 0x00020000u32, // tfClearNoRipple
                }));
            }
        } else if role == "user" && !has_no_ripple {
            problems.push(Value::String(format!(
                "You should set the no ripple flag on your {currency} line to {peer_account}"
            )));
            if include_transactions {
                transactions.push(serde_json::json!({
                    "TransactionType": "TrustSet",
                    "Account": account_str,
                    "LimitAmount": {
                        "currency": currency,
                        "issuer": peer_account,
                        "value": entry.get(if is_high { "HighLimit" } else { "LowLimit" })
                            .and_then(|v| v.get("value").and_then(|v| v.as_str()))
                            .unwrap_or("0"),
                    },
                    "Flags": 0x00020000u32, // tfSetNoRipple (131072)
                }));
            }
        }
    }

    let mut result = serde_json::json!({
        "account": account_str,
        "problems": problems,
        "ledger_index": ledger.header.sequence,
    });

    if include_transactions {
        result["transactions"] = Value::Array(transactions);
    }

    Ok(result)
}
