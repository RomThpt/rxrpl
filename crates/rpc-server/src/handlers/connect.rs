use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Initiate a peer connection to the specified IP and port.
///
/// This is an admin command that instructs the server to attempt
/// a P2P connection to the given address.
pub async fn connect(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ip = params
        .get("ip")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'ip'".into()))?;

    let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(51235);

    tracing::info!("connect requested to {ip}:{port}");

    let addr = format!("{ip}:{port}");
    let dispatched =
        ctx.send_overlay_command(rxrpl_overlay::command::OverlayCommand::ConnectTo { addr });
    if !dispatched {
        return Err(RpcServerError::Internal(
            "peer connections require network mode (no overlay attached)".into(),
        ));
    }

    Ok(serde_json::json!({
        "connecting": true,
        "ip": ip,
        "port": port,
        "message": "connection attempt initiated",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_config::ServerConfig;
    use rxrpl_overlay::command::OverlayCommand;

    #[tokio::test]
    async fn connect_without_overlay_errors() {
        let ctx = ServerContext::new(ServerConfig::default());
        let err = connect(serde_json::json!({"ip": "10.0.0.1", "port": 51235}), &ctx)
            .await
            .expect_err("standalone mode has no overlay");
        assert!(matches!(err, RpcServerError::Internal(_)));
    }

    #[tokio::test]
    async fn connect_dispatches_connect_to_command() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = ServerContext::new(ServerConfig::default());
        ctx.attach_overlay_command(tx);

        let resp = connect(serde_json::json!({"ip": "10.0.0.2", "port": 5000}), &ctx)
            .await
            .expect("dispatch succeeds when overlay is attached");
        assert_eq!(resp["connecting"], serde_json::json!(true));

        match rx.try_recv().expect("a command was sent") {
            OverlayCommand::ConnectTo { addr } => assert_eq!(addr, "10.0.0.2:5000"),
            _ => panic!("expected ConnectTo"),
        }
    }

    #[tokio::test]
    async fn connect_missing_ip_is_invalid_params() {
        let ctx = ServerContext::new(ServerConfig::default());
        let err = connect(serde_json::json!({"port": 51235}), &ctx)
            .await
            .expect_err("ip is required");
        assert!(matches!(err, RpcServerError::InvalidParams(_)));
    }
}
