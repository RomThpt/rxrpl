use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// XRPL NetClock epoch (2000-01-01 UTC) in Unix seconds. Mirrors
/// `rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET` but kept inline so this
/// crate doesn't need a `rxrpl-ledger` dependency just for the constant.
const RIPPLE_EPOCH_OFFSET: u64 = 946_684_800;

/// Seconds elapsed since `close_time` (NetClock seconds since 2000-01-01).
/// Returns 0 when `close_time` is unset (catchup-only validation snapshot
/// before the matching ledger lands locally) or ahead of wall clock.
fn ledger_age(close_time: u32) -> u64 {
    if close_time == 0 {
        return 0;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(RIPPLE_EPOCH_OFFSET);
    now.saturating_sub(close_time as u64)
}

/// Snapshot of "what's in the closed ledger window" used by both
/// `server_info` and `server_state`.
struct ClosedLedgersSummary {
    complete_ledgers: String,
    last_seq: u32,
    validated_ledger: Option<Value>,
}

/// Format an ascending list of sequence numbers as rippled-compatible
/// `complete_ledgers` segments: contiguous runs collapse to `"start-end"`
/// and disjoint runs are joined with commas — e.g. `[1,2,3,5,7,8]` becomes
/// `"1-3,5-5,7-8"`. Empty input returns `"empty"`.
///
/// Why: a deque that received `push_back` only for consensus-closed ledgers
/// (and skipped catchup-adopted ones) is not a contiguous range. Reporting
/// `"first-last"` lies to RPC consumers like the confluence dashboard, which
/// then 404s when fetching an intermediate seq.
pub(crate) fn format_ledger_ranges(seqs: &[u32]) -> String {
    if seqs.is_empty() {
        return "empty".to_string();
    }
    let mut out = String::new();
    let mut start = seqs[0];
    let mut prev = seqs[0];
    for &s in &seqs[1..] {
        if s == prev {
            continue;
        }
        if s == prev + 1 {
            prev = s;
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&format!("{start}-{prev}"));
        start = s;
        prev = s;
    }
    if !out.is_empty() {
        out.push(',');
    }
    out.push_str(&format!("{start}-{prev}"));
    out
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
        let mut seqs: Vec<u32> = closed.iter().map(|l| l.header.sequence).collect();
        seqs.sort_unstable();
        match net {
            Some(snap) => {
                let cap = snap.seq;
                if cap < first {
                    return ClosedLedgersSummary {
                        complete_ledgers: "empty".to_string(),
                        last_seq: 1,
                        validated_ledger: None,
                    };
                }
                seqs.retain(|s| *s <= cap);
                let validated = serde_json::json!({
                    "seq": snap.seq,
                    "hash": snap.hash.to_string(),
                    "close_time": snap.close_time,
                    "age": ledger_age(snap.close_time),
                    "base_fee_xrp": 0.00001,
                    "reserve_base_xrp": 10,
                    "reserve_inc_xrp": 2,
                });
                ClosedLedgersSummary {
                    complete_ledgers: format_ledger_ranges(&seqs),
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
                    "age": ledger_age(last_ledger.header.close_time),
                    "base_fee_xrp": 0.00001,
                    "reserve_base_xrp": 10,
                    "reserve_inc_xrp": 2,
                });
                ClosedLedgersSummary {
                    complete_ledgers: format_ledger_ranges(&seqs),
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

/// Feeds the overlay's peer `/crawl` endpoint the same server-level data that
/// `server_info` reports. Kept sync (the crawl is served from the accept path),
/// so the ledger window is read non-blocking via `try_read`.
impl rxrpl_overlay::crawl::CrawlInfo for ServerContext {
    fn crawl_snapshot(&self) -> rxrpl_overlay::crawl::CrawlServerSnapshot {
        let server_state = if self.local_manifest().is_some() {
            "proposing"
        } else {
            "full"
        };
        let complete_ledgers = self
            .closed_ledgers
            .as_ref()
            .and_then(|cl| cl.try_read().ok())
            .map(|guard| {
                let mut seqs: Vec<u32> = guard.iter().map(|l| l.header.sequence).collect();
                seqs.sort_unstable();
                format_ledger_ranges(&seqs)
            })
            .unwrap_or_else(|| "empty".to_string());

        rxrpl_overlay::crawl::CrawlServerSnapshot {
            build_version: env!("CARGO_PKG_VERSION").to_string(),
            server_state: server_state.to_string(),
            complete_ledgers,
            uptime_secs: self.uptime_seconds(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_ranges_empty() {
        assert_eq!(format_ledger_ranges(&[]), "empty");
    }

    #[test]
    fn format_ranges_singleton() {
        assert_eq!(format_ledger_ranges(&[1]), "1-1");
        assert_eq!(format_ledger_ranges(&[42]), "42-42");
    }

    #[test]
    fn format_ranges_contiguous() {
        assert_eq!(format_ledger_ranges(&[1, 2, 3, 4, 5]), "1-5");
        assert_eq!(format_ledger_ranges(&[10, 11, 12]), "10-12");
    }

    #[test]
    fn format_ranges_with_gaps() {
        assert_eq!(format_ledger_ranges(&[1, 3, 5]), "1-1,3-3,5-5");
        assert_eq!(format_ledger_ranges(&[1, 2, 3, 7, 8, 9]), "1-3,7-9");
        assert_eq!(
            format_ledger_ranges(&[1, 2, 4, 6, 7, 10]),
            "1-2,4-4,6-7,10-10"
        );
    }

    #[test]
    fn format_ranges_alternating_pattern_from_kurtosis() {
        let seqs: Vec<u32> = vec![
            1, 3, 5, 7, 8, 9, 11, 12, 15, 18, 19, 22, 23, 26, 27, 29, 30, 32, 33,
        ];
        assert_eq!(
            format_ledger_ranges(&seqs),
            "1-1,3-3,5-5,7-9,11-12,15-15,18-19,22-23,26-27,29-30,32-33"
        );
    }

    #[test]
    fn format_ranges_handles_duplicates() {
        assert_eq!(format_ledger_ranges(&[1, 1, 2, 2, 3]), "1-3");
    }
}
