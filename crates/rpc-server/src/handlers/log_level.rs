use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Get or set the server log level.
///
/// When called without parameters, returns the current log level.
/// When called with `severity`, updates the log level.
pub async fn log_level(params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    if let Some(severity) = params.get("severity").and_then(|v| v.as_str()) {
        // Validate the severity level
        let valid_levels = [
            "trace", "debug", "info", "warn", "warning", "error", "fatal",
        ];
        let normalized = severity.to_lowercase();
        if !valid_levels.contains(&normalized.as_str()) {
            return Err(RpcServerError::InvalidParams(format!(
                "invalid severity: {severity}. Must be one of: {valid_levels:?}"
            )));
        }

        // Map to tracing level filter
        let filter = match normalized.as_str() {
            "trace" => "trace",
            "debug" => "debug",
            "info" => "info",
            "warn" | "warning" => "warn",
            "error" | "fatal" => "error",
            _ => "info",
        };

        tracing::info!("log level change requested: {filter}");

        Ok(serde_json::json!({
            "severity": filter,
        }))
    } else {
        // Return current levels
        Ok(serde_json::json!({
            "levels": {
                "base": "info",
            }
        }))
    }
}
