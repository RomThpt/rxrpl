use reqwest::Client;
use serde_json::Value;

use rxrpl_rpc_api::types::JsonRpcRequest;

use crate::error::ClientError;

/// HTTP transport for JSON-RPC requests.
pub struct HttpTransport {
    client: Client,
    url: String,
}

impl HttpTransport {
    pub fn new(url: impl Into<String>) -> Result<Self, ClientError> {
        let client = Client::builder().build().map_err(ClientError::from)?;
        Ok(Self {
            client,
            url: url.into(),
        })
    }

    pub fn with_client(client: Client, url: impl Into<String>) -> Self {
        Self {
            client,
            url: url.into(),
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let req = JsonRpcRequest::new(method, params);

        let resp = self
            .client
            .post(&self.url)
            .json(&req)
            .send()
            .await
            .map_err(ClientError::from)?;

        let body: Value = resp.json().await.map_err(ClientError::from)?;

        // Extract the result field
        if let Some(result) = body.get("result") {
            // Check for error in result
            if let Some(error) = result.get("error") {
                let error_str = error.as_str().unwrap_or("unknown").to_string();
                let error_code = result
                    .get("error_code")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(-1) as i32;
                let error_message = result
                    .get("error_message")
                    .and_then(|m| m.as_str())
                    .map(String::from);
                return Err(ClientError::Rpc {
                    error: error_str,
                    code: error_code,
                    message: error_message,
                });
            }
            Ok(result.clone())
        } else {
            Ok(body)
        }
    }
}
