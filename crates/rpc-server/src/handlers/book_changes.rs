use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Return offer changes for a specific ledger.
///
/// Scans transactions in the ledger and collects changes to offer book entries.
pub async fn book_changes(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    // Collect all transactions and look for offer-related metadata
    let mut changes: Vec<Value> = Vec::new();
    let mut books_seen: BTreeMap<String, Vec<Value>> = BTreeMap::new();

    for (hash, data) in ledger.tx_map.iter() {
        let record: Value = match serde_json::from_slice(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Look for AffectedNodes in metadata
        let meta = match record.get("meta").or_else(|| record.get("metaData")) {
            Some(m) => m,
            None => continue,
        };

        let affected = match meta.get("AffectedNodes").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => continue,
        };

        for node in affected {
            // Check for offer modifications
            let (action, fields) = if let Some(modified) = node.get("ModifiedNode") {
                if modified.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                    continue;
                }
                ("modified", modified)
            } else if let Some(deleted) = node.get("DeletedNode") {
                if deleted.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                    continue;
                }
                ("deleted", deleted)
            } else if let Some(created) = node.get("CreatedNode") {
                if created.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                    continue;
                }
                ("created", created)
            } else {
                continue;
            };

            let final_fields = fields
                .get("FinalFields")
                .or_else(|| fields.get("NewFields"))
                .unwrap_or(&Value::Null);

            let taker_pays = final_fields.get("TakerPays");
            let taker_gets = final_fields.get("TakerGets");

            let pays_currency = extract_currency(taker_pays);
            let gets_currency = extract_currency(taker_gets);
            let book_key = format!("{pays_currency}/{gets_currency}");

            let change = serde_json::json!({
                "tx_hash": hash.to_string(),
                "action": action,
                "taker_pays": taker_pays,
                "taker_gets": taker_gets,
            });

            books_seen.entry(book_key).or_default().push(change);
        }
    }

    for (book, book_changes) in books_seen {
        changes.push(serde_json::json!({
            "currency_pair": book,
            "changes": book_changes,
        }));
    }

    Ok(serde_json::json!({
        "type": "bookChanges",
        "ledger_index": ledger.header.sequence,
        "ledger_hash": ledger.header.hash.to_string(),
        "changes": changes,
    }))
}

fn extract_currency(amount: Option<&Value>) -> String {
    match amount {
        Some(Value::String(_)) => "XRP".to_string(),
        Some(obj) => obj
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("???")
            .to_string(),
        None => "???".to_string(),
    }
}
