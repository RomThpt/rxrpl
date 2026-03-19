use serde_json::Value;

/// Build a JSON params object for dispatch from a protobuf request with a single account field.
pub fn account_params(account: &str) -> Value {
    serde_json::json!({"account": account})
}

/// Build JSON params for account_tx with optional limit and marker.
pub fn account_tx_params(account: &str, limit: u32, marker: &str) -> Value {
    let mut params = serde_json::json!({"account": account});
    if limit > 0 {
        params["limit"] = Value::from(limit);
    }
    if !marker.is_empty() {
        params["marker"] = Value::String(marker.to_string());
    }
    params
}

/// Build JSON params for submit with tx_blob or tx_json.
pub fn submit_params(tx_blob: &str, tx_json: &str) -> Value {
    if !tx_blob.is_empty() {
        serde_json::json!({"tx_blob": tx_blob})
    } else if !tx_json.is_empty() {
        match serde_json::from_str::<Value>(tx_json) {
            Ok(v) => serde_json::json!({"tx_json": v}),
            Err(_) => serde_json::json!({"tx_json": tx_json}),
        }
    } else {
        serde_json::json!({})
    }
}

/// Build JSON params for tx lookup.
pub fn tx_params(transaction: &str) -> Value {
    serde_json::json!({"transaction": transaction})
}

/// Wrap a JSON result into a string for protobuf response.
pub fn json_to_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_params_format() {
        let p = account_params("rTest");
        assert_eq!(p["account"], "rTest");
    }

    #[test]
    fn account_tx_params_with_marker() {
        let p = account_tx_params("rTest", 100, "10:0");
        assert_eq!(p["account"], "rTest");
        assert_eq!(p["limit"], 100);
        assert_eq!(p["marker"], "10:0");
    }

    #[test]
    fn account_tx_params_without_optional() {
        let p = account_tx_params("rTest", 0, "");
        assert_eq!(p["account"], "rTest");
        assert!(p.get("limit").is_none());
        assert!(p.get("marker").is_none());
    }

    #[test]
    fn submit_params_blob() {
        let p = submit_params("1200...", "");
        assert_eq!(p["tx_blob"], "1200...");
    }

    #[test]
    fn submit_params_json() {
        let p = submit_params("", r#"{"Account":"rTest"}"#);
        assert_eq!(p["tx_json"]["Account"], "rTest");
    }

    #[test]
    fn json_to_string_roundtrip() {
        let v = serde_json::json!({"key": "value"});
        let s = json_to_string(&v);
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, v);
    }
}
