use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn account_info(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let account = params
        .get("account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'account' field".into()))?;

    let ledger = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;

    let account_id = decode_account_id(account).map_err(|_| RpcServerError::AccountMalformed)?;
    let key = keylet::account(&account_id);

    let ledger = ledger.read().await;

    let data = ledger
        .get_state(&key)
        .ok_or(RpcServerError::AccountNotFound)?;

    let account_data: Value = crate::handlers::common::decode_state_value(data)?;

    Ok(serde_json::json!({
        "account_data": account_data,
        "ledger_current_index": ledger.header.sequence,
    }))
}
