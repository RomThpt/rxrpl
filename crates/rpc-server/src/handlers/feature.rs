use std::sync::Arc;

use serde_json::Value;

use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

pub async fn feature(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let amendments_key = keylet::amendments();
    let data = match ledger.get_state(&amendments_key) {
        Some(d) => d,
        None => {
            return Ok(serde_json::json!({
                "features": {},
            }));
        }
    };

    let amendments: Value = crate::handlers::common::decode_state_value(data)?;

    let mut features = serde_json::Map::new();

    // Enabled amendments are in the "Amendments" array
    if let Some(enabled) = amendments.get("Amendments").and_then(|v| v.as_array()) {
        for amendment in enabled {
            if let Some(hash) = amendment.as_str() {
                features.insert(
                    hash.to_string(),
                    serde_json::json!({
                        "enabled": true,
                    }),
                );
            }
        }
    }

    // Pending/vetoed amendments are in "Majorities" array
    if let Some(majorities) = amendments.get("Majorities").and_then(|v| v.as_array()) {
        for majority in majorities {
            if let Some(m) = majority.get("Majority") {
                if let Some(hash) = m.get("Amendment").and_then(|v| v.as_str()) {
                    features.entry(hash.to_string()).or_insert_with(|| {
                        serde_json::json!({
                            "enabled": false,
                            "supported": true,
                        })
                    });
                }
            }
        }
    }

    Ok(serde_json::json!({
        "features": features,
    }))
}
