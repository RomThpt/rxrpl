use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return consensus state information.
///
/// Returns the current consensus phase and related timing information.
/// Full consensus details will be available once the consensus engine
/// is fully integrated with the RPC layer.
pub async fn consensus_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger_seq = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        l.header.sequence
    } else {
        0
    };

    let validated_seq = if let Some(ref closed) = ctx.closed_ledgers {
        let closed = closed.read().await;
        closed.back().map(|l| l.header.sequence).unwrap_or(0)
    } else {
        0
    };

    // Active proposer when a validator_identity is configured (then the
    // consensus engine signs and broadcasts ProposeSets every establish
    // round). Otherwise we observe the network as a passive validator.
    let proposing = ctx.local_manifest().is_some();
    let consensus_state = if proposing { "proposing" } else { "observing" };

    Ok(serde_json::json!({
        "info": {
            "consensus": consensus_state,
            "ledger_seq": ledger_seq,
            "our_position": {
                "proposers": 0,
            },
            "proposing": proposing,
            "validated_ledger": validated_seq,
        }
    }))
}
