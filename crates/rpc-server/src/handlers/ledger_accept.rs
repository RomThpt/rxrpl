use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;
use rxrpl_codec::address::decode_account_id;
use rxrpl_ledger::Ledger;
use rxrpl_storage::TxStore;
use rxrpl_txq::{FeeLevel, QueueEntry};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Index every transaction in a closed ledger into the tx store, plus
/// per-account indexes (sender + destination). Mirrors the loop run by
/// `Node::index_ledger_transactions` in the natural-close path so that
/// `account_tx`/`tx` queries work in standalone (`ledger_accept`) mode too.
fn index_closed_ledger(store: &dyn TxStore, ledger: &Ledger) {
    let seq = ledger.header.sequence;
    let mut tx_index = 0u32;

    ledger.tx_map.for_each(&mut |tx_hash, data| {
        if let Ok(record) = serde_json::from_slice::<Value>(data) {
            let meta_blob =
                serde_json::to_vec(record.get("meta").unwrap_or(&Value::Null)).unwrap_or_default();

            if let Err(e) =
                store.insert_transaction(tx_hash.as_bytes(), seq, tx_index, data, &meta_blob)
            {
                tracing::error!("failed to index tx {}: {}", tx_hash, e);
            }

            for field in ["Account", "Destination"] {
                if let Some(addr) = record
                    .get("tx_json")
                    .and_then(|tj| tj.get(field))
                    .and_then(|a| a.as_str())
                {
                    if let Ok(id) = decode_account_id(addr) {
                        if let Err(e) = store.insert_account_transaction(
                            id.as_bytes(),
                            seq,
                            tx_index,
                            tx_hash.as_bytes(),
                        ) {
                            tracing::error!("failed to index account tx ({field}): {e}");
                        }
                    }
                }
            }
        }

        tx_index += 1;
    });
}

/// Close the current ledger in standalone mode and open a new one.
///
/// After closing, queued transactions are re-applied against the fresh open
/// ledger in sequence order within each account, with accounts processed in
/// fee-priority order (highest fee first). Transactions that still succeed are
/// re-queued; the rest are silently dropped.
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

    // Snapshot the set of confirmed tx hashes from the just-closed ledger
    // before we move it into the history; used below to purge confirmed
    // entries from the retry queue.
    let mut confirmed_hashes: std::collections::HashSet<rxrpl_primitives::Hash256> =
        std::collections::HashSet::new();
    let mut txn_count: u32 = 0;
    closed_copy.tx_map.iter_ref().for_each(|(hash, data)| {
        confirmed_hashes.insert(*hash);
        txn_count += 1;
        // Mirror what the natural-close loop in node.rs does so subscribers
        // on the `transactions` stream get notified for txs that were applied
        // in a manually-closed (ledger_accept) ledger.
        if let Ok(record) = serde_json::from_slice::<serde_json::Value>(data) {
            let tx_json = record.get("tx_json").cloned().unwrap_or_default();
            let _ = ctx
                .event_sender()
                .send(crate::events::ServerEvent::TransactionValidated {
                    transaction: tx_json,
                    meta: record.get("meta").cloned().unwrap_or_default(),
                    ledger_index: closed_seq,
                });
        }
    });

    // Index transactions into the tx_store so account_tx/tx RPC queries
    // can find them. The natural-close loop in node.rs does this; in
    // standalone mode (ledger_accept-driven) we must do it here too.
    if let Some(ref store) = ctx.tx_store {
        index_closed_ledger(store.as_ref(), &closed_copy);
    }

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
            txn_count,
        });

    // Replace the open ledger
    *ledger = new_open;
    let new_seq = ledger.header.sequence;

    // --- Queue retry: re-apply queued txs against the new open ledger ---
    if let (Some(tq), Some(engine), Some(fees)) = (&ctx.tx_queue, &ctx.tx_engine, &ctx.fees) {
        let pending = {
            let mut q = tq.write().await;
            // Drop transactions that are already confirmed in the just-closed
            // ledger's tx_map, plus anything past its LastLedgerSequence. The
            // remaining queue entries (preflight-passed but not yet applied)
            // are drained for retry against the new open ledger.
            q.remove_if(|hash| confirmed_hashes.contains(hash));
            q.remove_expired(new_seq);
            q.drain_for_retry_ordered()
        };

        let rules = Rules::new();
        let mut requeue = Vec::new();
        let mut applied_count: u64 = 0;
        let mut dropped_count: u64 = 0;

        for (_account, entries) in pending {
            for entry in entries {
                match engine.apply(&entry.tx, &mut ledger, &rules, fees) {
                    Ok(result) if result.is_success() => {
                        let fee_drops = entry
                            .tx
                            .get("Fee")
                            .and_then(|f| f.as_str())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(0);
                        requeue.push(QueueEntry {
                            hash: entry.hash,
                            tx: entry.tx,
                            fee_level: FeeLevel::new(fee_drops, fees.base_fee),
                            account: entry.account,
                            sequence: entry.sequence,
                            last_ledger_sequence: entry.last_ledger_sequence,
                            preflight_passed: entry.preflight_passed,
                        });
                        applied_count += 1;
                    }
                    _ => {
                        // Transaction no longer valid -- drop it silently.
                        dropped_count += 1;
                    }
                }
            }
        }

        // Re-insert surviving entries and update metrics.
        let mut q = tq.write().await;
        for _ in 0..applied_count {
            q.record_applied();
        }
        for _ in 0..dropped_count {
            q.record_drop();
        }
        for entry in requeue {
            let _ = q.submit(entry);
        }
    }

    Ok(serde_json::json!({
        "ledger_current_index": new_seq,
    }))
}
