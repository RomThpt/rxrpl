use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_ledger::Ledger;
use rxrpl_pathfind::{PathAlternative, PathRequest, parse_amount_issue, path_step_to_json};

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;
use crate::subscriptions::PathFindSubscription;

/// Build a `PathFindSubscription` from raw JSON params, validating inputs.
pub fn parse_path_find_params(params: &Value) -> Result<PathFindSubscription, RpcServerError> {
    let source_str = params
        .get("source_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'source_account'".into()))?;

    let dest_str = params
        .get("destination_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'destination_account'".into()))?;

    let dest_amount = params
        .get("destination_amount")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'destination_amount'".into()))?;

    let source = decode_account_id(source_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid source_account: {e}")))?;

    let destination = decode_account_id(dest_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid destination_account: {e}")))?;

    let source_currencies =
        if let Some(arr) = params.get("source_currencies").and_then(|v| v.as_array()) {
            let mut issues = Vec::new();
            for item in arr {
                if let Some(issue) = parse_amount_issue(item) {
                    issues.push(issue);
                }
            }
            Some(issues)
        } else {
            None
        };

    Ok(PathFindSubscription {
        source,
        destination,
        destination_amount: dest_amount,
        source_currencies,
        last_result: None,
        source_account_str: source_str.to_string(),
        destination_account_str: dest_str.to_string(),
    })
}

/// Run the pathfinding algorithm for a subscription against a given ledger.
///
/// Returns the alternatives JSON array and the serialized form for dedup.
pub fn run_path_find(sub: &PathFindSubscription, ledger: &Ledger) -> (Vec<Value>, String) {
    let request = PathRequest {
        source: sub.source,
        destination: sub.destination,
        destination_amount: sub.destination_amount.clone(),
        source_currencies: sub.source_currencies.clone(),
    };

    let alternatives = request.find_paths(ledger);
    let alts_json = alternatives_to_json(&alternatives);
    let serialized = serde_json::to_string(&alts_json).unwrap_or_default();
    (alts_json, serialized)
}

/// Convert `PathAlternative` results to JSON array.
fn alternatives_to_json(alternatives: &[PathAlternative]) -> Vec<Value> {
    alternatives
        .iter()
        .map(|alt| {
            let paths: Vec<Value> = alt
                .paths_computed
                .iter()
                .map(|path| {
                    let steps: Vec<Value> = path.iter().map(path_step_to_json).collect();
                    Value::Array(steps)
                })
                .collect();

            serde_json::json!({
                "source_amount": alt.source_amount,
                "paths_computed": paths,
            })
        })
        .collect()
}

/// Build the full response JSON for a path_find create result.
pub fn build_path_find_response(sub: &PathFindSubscription, alts_json: &[Value]) -> Value {
    serde_json::json!({
        "alternatives": alts_json,
        "destination_account": sub.destination_account_str,
        "destination_amount": sub.destination_amount,
        "source_account": sub.source_account_str,
        "full_reply": true,
    })
}

/// HTTP/one-shot path_find handler.
///
/// Over HTTP this behaves as a single-shot request. The streaming
/// create/close/status subcommands are handled directly in the
/// WebSocket connection loop.
pub async fn path_find(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let subcommand = params
        .get("subcommand")
        .and_then(|v| v.as_str())
        .unwrap_or("create");

    match subcommand {
        "create" => {
            let ledger = resolve_ledger(&params, ctx).await?;
            let sub = parse_path_find_params(&params)?;
            let (alts_json, _serialized) = run_path_find(&sub, &ledger);
            Ok(build_path_find_response(&sub, &alts_json))
        }
        "close" => Ok(serde_json::json!({ "closed": true })),
        "status" => Ok(serde_json::json!({ "status": "no path_find in progress" })),
        _ => Err(RpcServerError::InvalidParams(format!(
            "unknown subcommand: {subcommand}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_path_find_params() {
        let params = serde_json::json!({
            "source_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_amount": "1000000",
        });
        let sub = parse_path_find_params(&params).unwrap();
        assert_eq!(sub.source_account_str, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
        assert_eq!(
            sub.destination_account_str,
            "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"
        );
        assert_eq!(sub.destination_amount, serde_json::json!("1000000"));
        assert!(sub.source_currencies.is_none());
        assert!(sub.last_result.is_none());
    }

    #[test]
    fn parse_missing_source_account() {
        let params = serde_json::json!({
            "destination_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_amount": "1000000",
        });
        assert!(parse_path_find_params(&params).is_err());
    }

    #[test]
    fn parse_missing_destination_account() {
        let params = serde_json::json!({
            "source_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_amount": "1000000",
        });
        assert!(parse_path_find_params(&params).is_err());
    }

    #[test]
    fn parse_missing_destination_amount() {
        let params = serde_json::json!({
            "source_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        });
        assert!(parse_path_find_params(&params).is_err());
    }

    #[test]
    fn parse_invalid_source_account() {
        let params = serde_json::json!({
            "source_account": "not_valid",
            "destination_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_amount": "1000000",
        });
        assert!(parse_path_find_params(&params).is_err());
    }

    #[test]
    fn parse_with_source_currencies() {
        let params = serde_json::json!({
            "source_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "destination_amount": "1000000",
            "source_currencies": [
                {"currency": "USD", "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"},
            ],
        });
        let sub = parse_path_find_params(&params).unwrap();
        assert!(sub.source_currencies.is_some());
        assert_eq!(sub.source_currencies.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn run_path_find_on_genesis_ledger() {
        let sub = PathFindSubscription {
            source: rxrpl_primitives::AccountId([1u8; 20]),
            destination: rxrpl_primitives::AccountId([2u8; 20]),
            destination_amount: serde_json::json!("1000000"),
            source_currencies: None,
            last_result: None,
            source_account_str: "rSource".into(),
            destination_account_str: "rDest".into(),
        };
        let ledger = rxrpl_ledger::Ledger::genesis();
        let (alts, serialized) = run_path_find(&sub, &ledger);
        // Genesis ledger has no trust lines, so XRP-to-XRP returns an empty-path alternative.
        assert!(!serialized.is_empty());
        // The result is deterministic.
        let (alts2, serialized2) = run_path_find(&sub, &ledger);
        assert_eq!(serialized, serialized2);
        assert_eq!(alts.len(), alts2.len());
    }

    #[test]
    fn build_response_includes_required_fields() {
        let sub = PathFindSubscription {
            source: rxrpl_primitives::AccountId([1u8; 20]),
            destination: rxrpl_primitives::AccountId([2u8; 20]),
            destination_amount: serde_json::json!("1000000"),
            source_currencies: None,
            last_result: None,
            source_account_str: "rSource".into(),
            destination_account_str: "rDest".into(),
        };
        let alts = vec![serde_json::json!({"source_amount": "1000000", "paths_computed": []})];
        let resp = build_path_find_response(&sub, &alts);
        assert_eq!(resp["source_account"], "rSource");
        assert_eq!(resp["destination_account"], "rDest");
        assert_eq!(resp["destination_amount"], "1000000");
        assert_eq!(resp["full_reply"], true);
        assert!(resp["alternatives"].is_array());
    }
}
