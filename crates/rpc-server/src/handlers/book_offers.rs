use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{parse_currency_issuer, resolve_ledger};

pub async fn book_offers(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let taker_pays = params
        .get("taker_pays")
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'taker_pays'".into()))?;
    let taker_gets = params
        .get("taker_gets")
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'taker_gets'".into()))?;

    let (pays_currency, pays_issuer) =
        parse_currency_issuer(taker_pays).map_err(|_| RpcServerError::SourceCurrencyMalformed)?;
    let (gets_currency, gets_issuer) = parse_currency_issuer(taker_gets)?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(300)
        .min(400) as usize;

    let book_root = keylet::book_dir(&pays_currency, &pays_issuer, &gets_currency, &gets_issuer);

    let mut offers = Vec::new();

    // Walk the book directory pages
    let mut page = 0u64;
    'outer: loop {
        let page_key = keylet::dir_node(&book_root, page);
        let page_data = match ledger.get_state(&page_key) {
            Some(d) => d,
            None => break,
        };

        let page_json: Value = crate::handlers::common::decode_state_value(page_data)?;

        if let Some(indexes) = page_json.get("Indexes").and_then(|v| v.as_array()) {
            for idx_val in indexes {
                if offers.len() >= limit {
                    break 'outer;
                }
                let idx_str = idx_val.as_str().unwrap_or_default();
                let idx_hash: rxrpl_primitives::Hash256 = idx_str
                    .parse()
                    .map_err(|e| RpcServerError::Internal(format!("invalid index: {e}")))?;

                if let Some(entry_data) = ledger.get_state(&idx_hash) {
                    let entry: Value = crate::handlers::common::decode_state_value(entry_data)?;
                    offers.push(entry);
                }
            }
        }

        match page_json.get("IndexNext").and_then(|v| v.as_u64()) {
            Some(next) if next != 0 => page = next,
            _ => break,
        }
    }

    Ok(serde_json::json!({
        "offers": offers,
        "ledger_index": ledger.header.sequence,
    }))
}
