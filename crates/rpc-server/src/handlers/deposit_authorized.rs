use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Flag indicating that the account requires deposit authorization.
const LSF_DEPOSIT_AUTH: u32 = 0x01000000;

pub async fn deposit_authorized(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let source_str = params
        .get("source_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'source_account'".into()))?;
    let dest_str = params
        .get("destination_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'destination_account'".into()))?;

    let source_id = decode_account_id(source_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid source_account: {e}")))?;
    let dest_id = decode_account_id(dest_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid destination_account: {e}")))?;

    let ledger = resolve_ledger(&params, ctx).await?;

    // Get destination account root
    let dest_key = keylet::account(&dest_id);
    let dest_data = ledger
        .get_state(&dest_key)
        .ok_or_else(|| RpcServerError::InvalidParams("destination account not found".into()))?;

    let dest_account: Value = crate::handlers::common::decode_state_value(dest_data)?;

    let flags = dest_account
        .get("Flags")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let deposit_authorized = if (flags & LSF_DEPOSIT_AUTH) == 0 {
        // No deposit auth flag, anyone can deposit
        true
    } else {
        // Check if source has a deposit preauth from destination
        let preauth_key = keylet::deposit_preauth(&dest_id, &source_id);
        ledger.has_state(&preauth_key)
    };

    Ok(serde_json::json!({
        "source_account": source_str,
        "destination_account": dest_str,
        "deposit_authorized": deposit_authorized,
        "ledger_index": ledger.header.sequence,
    }))
}
