use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params: vec![params],
            id: Some(Value::Number(1.into())),
        }
    }

    pub fn with_id(mut self, id: Value) -> Self {
        self.id = Some(id);
        self
    }
}

/// JSON-RPC 2.0 response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<RpcWarning>>,
}

impl JsonRpcResponse {
    pub fn is_success(&self) -> bool {
        self.result
            .get("status")
            .and_then(|s| s.as_str())
            .map(|s| s == "success")
            .unwrap_or(false)
    }
}

/// RPC error structure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcError {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_exception: Option<String>,
}

/// RPC warning.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcWarning {
    pub id: i32,
    pub message: String,
}

/// WebSocket command (used for WS-based RPC).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsCommand {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(flatten)]
    pub params: Value,
}

/// WebSocket response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub response_type: Option<String>,
    #[serde(flatten)]
    pub result: Value,
}

/// Subscription stream types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamType {
    Ledger,
    Transactions,
    TransactionsProposed,
    Validations,
    Manifests,
    PeerStatus,
    ConsensusPhase,
    Server,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rpc_request_serialize() {
        let req = JsonRpcRequest::new("server_info", serde_json::json!({}));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("server_info"));
    }

    #[test]
    fn json_rpc_response_success() {
        let resp: JsonRpcResponse =
            serde_json::from_str(r#"{"result":{"status":"success","info":{}}}"#).unwrap();
        assert!(resp.is_success());
    }

    #[test]
    fn json_rpc_response_error() {
        let resp: JsonRpcResponse =
            serde_json::from_str(r#"{"result":{"status":"error","error":"actNotFound"}}"#).unwrap();
        assert!(!resp.is_success());
    }
}
