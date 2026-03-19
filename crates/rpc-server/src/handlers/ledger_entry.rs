use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

pub async fn ledger_entry(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let index = resolve_entry_index(&params)?;

    let data = ledger
        .get_state(&index)
        .ok_or_else(|| RpcServerError::InvalidParams("entry not found".into()))?;

    let node: Value = serde_json::from_slice(data)
        .map_err(|e| RpcServerError::Internal(format!("failed to deserialize entry: {e}")))?;

    Ok(serde_json::json!({
        "index": index.to_string(),
        "ledger_index": ledger.header.sequence,
        "node": node,
    }))
}

fn resolve_entry_index(params: &Value) -> Result<Hash256, RpcServerError> {
    // Direct index lookup
    if let Some(index) = params.get("index").and_then(|v| v.as_str()) {
        return Hash256::from_str(index)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid index: {e}")));
    }

    // account_root
    if let Some(account) = params.get("account_root").and_then(|v| v.as_str()) {
        let id = decode_account_id(account)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid account_root: {e}")))?;
        return Ok(keylet::account(&id));
    }

    // offer: { account, seq }
    if let Some(offer) = params.get("offer").and_then(|v| v.as_object()) {
        let account = offer
            .get("account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcServerError::InvalidParams("offer missing 'account'".into()))?;
        let seq = offer
            .get("seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcServerError::InvalidParams("offer missing 'seq'".into()))?;
        let id = decode_account_id(account)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid offer account: {e}")))?;
        return Ok(keylet::offer(&id, seq as u32));
    }

    // ripple_state: { accounts: [a, b], currency }
    if let Some(rs) = params.get("ripple_state").and_then(|v| v.as_object()) {
        let accounts = rs
            .get("accounts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                RpcServerError::InvalidParams("ripple_state missing 'accounts'".into())
            })?;
        if accounts.len() != 2 {
            return Err(RpcServerError::InvalidParams(
                "ripple_state accounts must have 2 entries".into(),
            ));
        }
        let a = decode_account_id(accounts[0].as_str().unwrap_or_default())
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid account[0]: {e}")))?;
        let b = decode_account_id(accounts[1].as_str().unwrap_or_default())
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid account[1]: {e}")))?;
        let currency = rs.get("currency").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcServerError::InvalidParams("ripple_state missing 'currency'".into())
        })?;
        let mut cur_bytes = [0u8; 20];
        if currency.len() == 3 {
            cur_bytes[12] = currency.as_bytes()[0];
            cur_bytes[13] = currency.as_bytes()[1];
            cur_bytes[14] = currency.as_bytes()[2];
        } else if currency.len() == 40 {
            let decoded = hex::decode(currency)
                .map_err(|e| RpcServerError::InvalidParams(format!("invalid currency hex: {e}")))?;
            cur_bytes.copy_from_slice(&decoded);
        }
        return Ok(keylet::trust_line(&a, &b, &cur_bytes));
    }

    // directory: { owner }
    if let Some(dir) = params.get("directory").and_then(|v| v.as_object()) {
        let owner = dir
            .get("owner")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcServerError::InvalidParams("directory missing 'owner'".into()))?;
        let id = decode_account_id(owner)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid directory owner: {e}")))?;
        return Ok(keylet::owner_dir(&id));
    }

    // check
    if let Some(check) = params.get("check").and_then(|v| v.as_str()) {
        return Hash256::from_str(check)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid check: {e}")));
    }

    // escrow: { owner, seq }
    if let Some(escrow) = params.get("escrow").and_then(|v| v.as_object()) {
        let owner = escrow
            .get("owner")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcServerError::InvalidParams("escrow missing 'owner'".into()))?;
        let seq = escrow
            .get("seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcServerError::InvalidParams("escrow missing 'seq'".into()))?;
        let id = decode_account_id(owner)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid escrow owner: {e}")))?;
        return Ok(keylet::escrow(&id, seq as u32));
    }

    // pay_channel
    if let Some(channel) = params.get("pay_channel").and_then(|v| v.as_str()) {
        return Hash256::from_str(channel)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid pay_channel: {e}")));
    }

    // deposit_preauth: { owner, authorized }
    if let Some(dp) = params.get("deposit_preauth").and_then(|v| v.as_object()) {
        let owner = dp.get("owner").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcServerError::InvalidParams("deposit_preauth missing 'owner'".into())
        })?;
        let authorized = dp
            .get("authorized")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                RpcServerError::InvalidParams("deposit_preauth missing 'authorized'".into())
            })?;
        let owner_id = decode_account_id(owner)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid dp owner: {e}")))?;
        let auth_id = decode_account_id(authorized)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid dp authorized: {e}")))?;
        return Ok(keylet::deposit_preauth(&owner_id, &auth_id));
    }

    // ticket: { account, seq }
    if let Some(ticket) = params.get("ticket").and_then(|v| v.as_object()) {
        let account = ticket
            .get("account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RpcServerError::InvalidParams("ticket missing 'account'".into()))?;
        let seq = ticket
            .get("ticket_seq")
            .or_else(|| ticket.get("seq"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| RpcServerError::InvalidParams("ticket missing 'ticket_seq'".into()))?;
        let id = decode_account_id(account)
            .map_err(|e| RpcServerError::InvalidParams(format!("invalid ticket account: {e}")))?;
        return Ok(keylet::ticket(&id, seq as u32));
    }

    Err(RpcServerError::InvalidParams(
        "must provide 'index' or a type-specific lookup (account_root, offer, ripple_state, directory, check, escrow, pay_channel, deposit_preauth, ticket)".into(),
    ))
}
