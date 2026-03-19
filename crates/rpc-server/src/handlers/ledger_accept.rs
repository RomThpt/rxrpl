use std::sync::Arc;

use serde_json::Value;

use rxrpl_ledger::Ledger;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Close the current ledger in standalone mode and open a new one.
pub async fn ledger_accept(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger_lock = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;

    let closed_lock = ctx
        .closed_ledgers
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;

    let mut ledger = ledger_lock.write().await;

    if !ledger.is_open() {
        return Err(RpcServerError::Internal("ledger is not open".into()));
    }

    // Close the current ledger with current unix time approximation
    let close_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    // XRPL epoch offset: 946684800 (2000-01-01)
    let ripple_close_time = close_time.saturating_sub(946_684_800);

    ledger
        .close(ripple_close_time, 0)
        .map_err(|e| RpcServerError::Internal(format!("failed to close ledger: {e}")))?;

    let closed_seq = ledger.header.sequence;
    let closed_hash = ledger.header.hash;

    // Push closed ledger to history
    let closed_copy = ledger.clone();
    let new_open = Ledger::new_open(&closed_copy);

    {
        let mut closed_ledgers = closed_lock.write().await;
        closed_ledgers.push_back(closed_copy);
    }

    // Emit ledger closed event
    let _ = ctx
        .event_sender()
        .send(crate::events::ServerEvent::LedgerClosed {
            ledger_index: closed_seq,
            ledger_hash: closed_hash,
            ledger_time: ripple_close_time,
            txn_count: 0,
        });

    // Replace the open ledger
    *ledger = new_open;

    Ok(serde_json::json!({
        "ledger_current_index": ledger.header.sequence,
    }))
}
