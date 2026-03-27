use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::{require_account_id, resolve_ledger, walk_owner_directory};

/// Return owner information for an account.
///
/// Deprecated rippled RPC that returns the objects owned by an account.
/// This is essentially a simplified view of `account_objects` and returns
/// the account's owner directory entries.
pub async fn owner_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;
    let ledger = resolve_ledger(&params, ctx).await?;

    let (entries, _marker) = walk_owner_directory(&ledger, &account_id, 200, None)?;

    let objects: Vec<Value> = entries.into_iter().map(|(_hash, val)| val).collect();

    Ok(serde_json::json!({
        "account_objects": objects,
        "ledger_current_index": ledger.header.sequence,
    }))
}
