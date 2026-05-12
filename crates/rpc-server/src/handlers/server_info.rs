use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Snapshot of "what's in the closed ledger window" used by both
/// `server_info` and `server_state`.
struct ClosedLedgersSummary {
    complete_ledgers: String,
    last_seq: u32,
    validated_ledger: Option<Value>,
}

async fn closed_ledgers_summary(ctx: &Arc<ServerContext>) -> ClosedLedgersSummary {
    // When a network-validated tip is published (networked mode after the
    // first quorum), it is authoritative: `validated_ledger.seq` must reflect
    // what the UNL has agreed on, and `complete_ledgers` is capped to that
    // tip so peers don't ask us for locally-closed-but-unvalidated ancestors.
    // In standalone (no slot attached) or before the first quorum is reached,
    // fall back to the locally-closed window — that's all the truth we have.
    let net = ctx.network_validated();
    if let Some(ref closed) = ctx.closed_ledgers {
        let closed = closed.read().await;
        if closed.is_empty() {
            return ClosedLedgersSummary {
                complete_ledgers: "empty".to_string(),
                last_seq: 1,
                validated_ledger: None,
            };
        }
        let first = closed.front().unwrap().header.sequence;
        match net {
            Some(snap) => {
                let cap = snap.seq;
                if cap < first {
                    // Validated tip is older than what we hold (shouldn't
                    // normally happen). Report empty rather than lie about
                    // a range we cannot serve as validated.
                    return ClosedLedgersSummary {
                        complete_ledgers: "empty".to_string(),
                        last_seq: 1,
                        validated_ledger: None,
                    };
                }
                let validated = serde_json::json!({
                    "seq": snap.seq,
                    "hash": snap.hash.to_string(),
                    "close_time": snap.close_time,
                    "base_fee_xrp": 0.00001,
                    "reserve_base_xrp": 10,
                    "reserve_inc_xrp": 2,
                });
                ClosedLedgersSummary {
                    complete_ledgers: format!("{first}-{cap}"),
                    last_seq: cap,
                    validated_ledger: Some(validated),
                }
            }
            None => {
                let last_ledger = closed.back().unwrap();
                let last = last_ledger.header.sequence;
                let validated = serde_json::json!({
                    "seq": last,
                    "hash": last_ledger.header.hash.to_string(),
                    "close_time": last_ledger.header.close_time,
                    "base_fee_xrp": 0.00001,
                    "reserve_base_xrp": 10,
                    "reserve_inc_xrp": 2,
                });
                ClosedLedgersSummary {
                    complete_ledgers: format!("{first}-{last}"),
                    last_seq: last,
                    validated_ledger: Some(validated),
                }
            }
        }
    } else {
        ClosedLedgersSummary {
            complete_ledgers: "empty".to_string(),
            last_seq: 1,
            validated_ledger: None,
        }
    }
}

pub async fn server_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let summary = closed_ledgers_summary(ctx).await;

    let current_index = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        l.header.sequence
    } else {
        summary.last_seq
    };

    // `proposing` / `am_validator` flag — true when a validator_identity is
    // configured. Mirrors rippled's `info.proposing` so dashboards and ops
    // tooling can tell whether this node emits ProposeSets in the consensus
    // round rather than just validating peer ledgers.
    // `server_state` follows rippled's convention: a fully-synced validator
    // that emits proposals reports "proposing"; a fully-synced read-only
    // node reports "full". The xrpl-confluence dashboard and other ops
    // tooling key off this string to display a node's role.
    let proposing = ctx.local_manifest().is_some();
    let server_state = if proposing { "proposing" } else { "full" };

    let mut info = serde_json::json!({
        "build_version": env!("CARGO_PKG_VERSION"),
        "server_state": server_state,
        "complete_ledgers": summary.complete_ledgers,
        "ledger_current_index": current_index,
        "proposing": proposing,
        "am_validator": proposing,
        "peers": ctx.peer_count(),
        "uptime": ctx.uptime_seconds(),
    });
    if let Some(lc) = ctx.last_close() {
        info["last_close"] = serde_json::json!({
            "proposers": lc.proposers,
            "converge_time_s": lc.converge_time_s,
        });
    }
    if let Some(v) = summary.validated_ledger {
        info["validated_ledger"] = v;
    }
    if let Some(handle) = ctx.domain_attestation_status.as_ref() {
        let snap = handle.read().await;
        if let Some(local) = snap.get("local") {
            info["domain_verification"] = local.clone();
        }
    }

    Ok(serde_json::json!({ "info": info }))
}

pub async fn server_state(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let summary = closed_ledgers_summary(ctx).await;
    let server_state = if ctx.local_manifest().is_some() {
        "proposing"
    } else {
        "full"
    };

    Ok(serde_json::json!({
        "state": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": server_state,
            "complete_ledgers": summary.complete_ledgers,
        }
    }))
}
