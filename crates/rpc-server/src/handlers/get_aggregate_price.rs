use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Aggregate price data from oracle ledger entries.
///
/// Reads oracle entries for each specified oracle and computes
/// aggregate statistics (mean, median, trimmed mean) for a given asset pair.
pub async fn get_aggregate_price(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let base_asset = params
        .get("base_asset")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'base_asset'".into()))?;

    let quote_asset = params
        .get("quote_asset")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'quote_asset'".into()))?;

    let trim = params
        .get("trim")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .min(25) as f64;

    let oracles = params
        .get("oracles")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'oracles' array".into()))?;

    let mut prices: Vec<f64> = Vec::new();

    for oracle_spec in oracles {
        let account = oracle_spec
            .get("account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcServerError::InvalidParams("oracle missing 'account'".into()))?;

        let oracle_document_id = oracle_spec
            .get("oracle_document_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                RpcServerError::InvalidParams("oracle missing 'oracle_document_id'".into())
            })? as u32;

        let account_id = decode_account_id(account)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid oracle account: {e}")))?;

        let oracle_key = keylet::oracle(&account_id, oracle_document_id);

        let data = match ledger.get_state(&oracle_key) {
            Some(d) => d,
            None => continue,
        };

        let oracle_entry: Value = serde_json::from_slice(data)
            .map_err(|e| RpcServerError::Internal(format!("failed to deserialize oracle: {e}")))?;

        // Search PriceDataSeries for matching asset pair
        if let Some(series) = oracle_entry
            .get("PriceDataSeries")
            .and_then(|v| v.as_array())
        {
            for entry in series {
                let pd = entry.get("PriceData").unwrap_or(entry);
                let entry_base = pd
                    .get("BaseAsset")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let entry_quote = pd
                    .get("QuoteAsset")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                if entry_base == base_asset && entry_quote == quote_asset {
                    if let Some(price) = pd.get("AssetPrice").and_then(|v| {
                        v.as_f64()
                            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                    }) {
                        let scale = pd.get("Scale").and_then(|v| v.as_u64()).unwrap_or(0);
                        let adjusted = price / 10f64.powi(scale as i32);
                        prices.push(adjusted);
                    }
                }
            }
        }
    }

    if prices.is_empty() {
        return Err(RpcServerError::InvalidParams(
            "no matching oracle data found for the specified asset pair".into(),
        ));
    }

    prices.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let entire_set_mean = prices.iter().sum::<f64>() / prices.len() as f64;
    let median = if prices.len() % 2 == 0 {
        (prices[prices.len() / 2 - 1] + prices[prices.len() / 2]) / 2.0
    } else {
        prices[prices.len() / 2]
    };

    // Trimmed mean: remove trim% from each end
    let trim_count = ((trim / 100.0) * prices.len() as f64).floor() as usize;
    let trimmed = if trim_count * 2 < prices.len() {
        let trimmed_prices = &prices[trim_count..prices.len() - trim_count];
        trimmed_prices.iter().sum::<f64>() / trimmed_prices.len() as f64
    } else {
        entire_set_mean
    };

    Ok(serde_json::json!({
        "entire_set": {
            "mean": format!("{entire_set_mean}"),
            "size": prices.len(),
        },
        "median": format!("{median}"),
        "trimmed_set": {
            "mean": format!("{trimmed}"),
            "size": prices.len().saturating_sub(trim_count * 2),
        },
        "ledger_index": ledger.header.sequence,
    }))
}
