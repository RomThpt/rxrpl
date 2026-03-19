use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Return vault entry info from the ledger.
///
/// Looks up a vault by either direct index or by owner + sequence.
pub async fn vault_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let vault_key = if let Some(index) = params.get("vault_id").and_then(|v| v.as_str()) {
        Hash256::from_str(index)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid vault_id: {e}")))?
    } else {
        let owner = params
            .get("owner")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RpcServerError::InvalidParams("must provide 'vault_id' or 'owner' + 'seq'".into())
            })?;
        let seq = params
            .get("seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcServerError::InvalidParams("missing 'seq'".into()))?
            as u32;

        let owner_id = decode_account_id(owner)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid owner: {e}")))?;
        keylet::vault(&owner_id, seq)
    };

    let data = ledger
        .get_state(&vault_key)
        .ok_or_else(|| RpcServerError::InvalidParams("vault not found".into()))?;

    let node: Value = serde_json::from_slice(data)
        .map_err(|e| RpcServerError::Internal(format!("failed to deserialize vault: {e}")))?;

    Ok(serde_json::json!({
        "vault_id": vault_key.to_string(),
        "ledger_index": ledger.header.sequence,
        "node": node,
    }))
}
