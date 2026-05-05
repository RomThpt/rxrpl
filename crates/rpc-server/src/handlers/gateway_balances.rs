use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

/// Aggregate trust line balances for a gateway/issuer account.
///
/// Groups balances by currency and optionally separates hot wallets.
pub async fn gateway_balances(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    // Parse optional hotwallet list
    let hotwallets: Vec<String> = match params.get("hotwallet") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    };

    let hotwallet_ids: Vec<_> = hotwallets
        .iter()
        .filter_map(|hw| decode_account_id(hw).ok())
        .collect();

    // Walk all owner directory entries
    let (entries, _) = walk_owner_directory(&ledger, &account_id, 10_000, None)?;

    // obligations: currency -> total amount owed by gateway
    let mut obligations: BTreeMap<String, f64> = BTreeMap::new();
    // assets: hotwallet_account -> currency -> balance
    let mut assets: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // frozen: currency -> total frozen
    let mut frozen: BTreeMap<String, f64> = BTreeMap::new();

    let account_str = params["account"].as_str().unwrap_or_default();

    for (_, entry) in &entries {
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

        let is_high = high_account == account_str;
        let peer_account = if is_high { low_account } else { high_account };

        let currency = entry
            .get("Balance")
            .and_then(|v| v.get("currency").and_then(|c| c.as_str()))
            .or_else(|| high_limit.get("currency").and_then(|c| c.as_str()))
            .unwrap_or("???")
            .to_string();

        let balance_str = entry
            .get("Balance")
            .and_then(|v| {
                v.get("value")
                    .and_then(|v| v.as_str())
                    .or_else(|| v.as_str())
            })
            .unwrap_or("0");

        let balance: f64 = balance_str.parse().unwrap_or(0.0);
        // From the gateway's perspective, negate if gateway is high account
        let gateway_balance = if is_high { -balance } else { balance };

        // Check if peer is a hotwallet
        let is_hotwallet = hotwallet_ids.iter().any(|hw_id| {
            decode_account_id(peer_account)
                .map(|pid| pid == *hw_id)
                .unwrap_or(false)
        });

        if is_hotwallet {
            // Positive balance from gateway's view = asset held by hotwallet
            let peer_balances = assets.entry(peer_account.to_string()).or_default();
            let display_val = if gateway_balance < 0.0 {
                format!("{}", -gateway_balance)
            } else {
                format!("{gateway_balance}")
            };
            peer_balances.insert(currency.clone(), display_val);
        }

        // Check for frozen
        let flags = entry.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);
        let high_freeze = flags & 0x00400000 != 0; // lsfHighFreeze
        let low_freeze = flags & 0x00200000 != 0; // lsfLowFreeze
        let is_frozen = if is_high { high_freeze } else { low_freeze };

        if is_frozen && gateway_balance < 0.0 {
            *frozen.entry(currency.clone()).or_insert(0.0) += -gateway_balance;
        }

        // Obligation: negative gateway_balance means gateway owes
        if gateway_balance < 0.0 {
            *obligations.entry(currency).or_insert(0.0) += -gateway_balance;
        }
    }

    // Build response
    let obligations_json: Value = obligations
        .into_iter()
        .map(|(cur, amount)| (cur, Value::String(format!("{amount}"))))
        .collect::<serde_json::Map<String, Value>>()
        .into();

    let assets_json: Value = if assets.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        assets
            .into_iter()
            .map(|(acct, balances)| {
                let bal_arr: Vec<Value> = balances
                    .into_iter()
                    .map(|(cur, val)| serde_json::json!({ "currency": cur, "value": val }))
                    .collect();
                (acct, Value::Array(bal_arr))
            })
            .collect::<serde_json::Map<String, Value>>()
            .into()
    };

    let frozen_json: Value = if frozen.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        frozen
            .into_iter()
            .map(|(cur, amount)| (cur, Value::String(format!("{amount}"))))
            .collect::<serde_json::Map<String, Value>>()
            .into()
    };

    Ok(serde_json::json!({
        "account": account_str,
        "obligations": obligations_json,
        "assets": assets_json,
        "frozen_balances": frozen_json,
        "ledger_index": ledger.header.sequence,
    }))
}
