use rxrpl_primitives::Hash256;
use serde_json::Value;

/// Events emitted by the server for WebSocket subscriptions.
#[derive(Clone, Debug)]
pub enum ServerEvent {
    LedgerClosed {
        ledger_index: u32,
        ledger_hash: Hash256,
        ledger_time: u32,
        txn_count: u32,
    },
    TransactionValidated {
        transaction: Value,
        meta: Value,
        ledger_index: u32,
    },
    TransactionProposed {
        transaction: Value,
        engine_result: String,
        engine_result_code: i32,
    },
    ValidationReceived {
        validator: String,
        ledger_hash: String,
        ledger_seq: u32,
        full: bool,
    },
    ManifestReceived {
        master_key: String,
        signing_key: String,
        seq: u32,
    },
    PeerStatusChange {
        peer_id: String,
        event: String,
    },
    ServerStateChange {
        state: String,
    },
    ConsensusPhaseChange {
        phase: String,
    },
    BookChange {
        taker_pays: Value,
        taker_gets: Value,
        open: String,
        close: String,
        high: String,
        low: String,
        volume: String,
    },
    PathFindUpdate {
        alternatives: Vec<Value>,
    },
}

/// Convert a server event to its JSON representation.
///
/// Events do NOT include an `id` field (XRPL convention: events have no id,
/// responses do).
pub fn event_to_json(event: &ServerEvent) -> Value {
    match event {
        ServerEvent::LedgerClosed {
            ledger_index,
            ledger_hash,
            ledger_time,
            txn_count,
        } => serde_json::json!({
            "type": "ledgerClosed",
            "ledger_index": ledger_index,
            "ledger_hash": ledger_hash.to_string(),
            "ledger_time": ledger_time,
            "txn_count": txn_count,
        }),
        ServerEvent::TransactionValidated {
            transaction,
            meta,
            ledger_index,
        } => serde_json::json!({
            "type": "transaction",
            "transaction": transaction,
            "meta": meta,
            "ledger_index": ledger_index,
            "validated": true,
            "status": "closed",
            "engine_result": "tesSUCCESS",
            "engine_result_code": 0,
        }),
        ServerEvent::TransactionProposed {
            transaction,
            engine_result,
            engine_result_code,
        } => serde_json::json!({
            "type": "transaction",
            "transaction": transaction,
            "validated": false,
            "status": "proposed",
            "engine_result": engine_result,
            "engine_result_code": engine_result_code,
        }),
        ServerEvent::ValidationReceived {
            validator,
            ledger_hash,
            ledger_seq,
            full,
        } => serde_json::json!({
            "type": "validationReceived",
            "validation_public_key": validator,
            "ledger_hash": ledger_hash,
            "ledger_index": ledger_seq,
            "full": full,
        }),
        ServerEvent::ManifestReceived {
            master_key,
            signing_key,
            seq,
        } => serde_json::json!({
            "type": "manifestReceived",
            "master_key": master_key,
            "signing_key": signing_key,
            "seq": seq,
        }),
        ServerEvent::PeerStatusChange { peer_id, event } => serde_json::json!({
            "type": "peerStatusChange",
            "peer": peer_id,
            "event": event,
        }),
        ServerEvent::ServerStateChange { state } => serde_json::json!({
            "type": "serverStatus",
            "server_status": state,
        }),
        ServerEvent::ConsensusPhaseChange { phase } => serde_json::json!({
            "type": "consensusPhase",
            "consensus": phase,
        }),
        ServerEvent::BookChange {
            taker_pays,
            taker_gets,
            open,
            close,
            high,
            low,
            volume,
        } => serde_json::json!({
            "type": "bookChanges",
            "taker_pays": taker_pays,
            "taker_gets": taker_gets,
            "open": open,
            "close": close,
            "high": high,
            "low": low,
            "vol": volume,
        }),
        ServerEvent::PathFindUpdate { alternatives } => serde_json::json!({
            "type": "path_find",
            "alternatives": alternatives,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_closed_event_json() {
        let event = ServerEvent::LedgerClosed {
            ledger_index: 42,
            ledger_hash: Hash256::default(),
            ledger_time: 1000,
            txn_count: 5,
        };
        let json = event_to_json(&event);
        assert_eq!(json["type"], "ledgerClosed");
        assert_eq!(json["ledger_index"], 42);
        assert_eq!(json["txn_count"], 5);
        assert!(json.get("id").is_none());
    }

    #[test]
    fn transaction_validated_event_json() {
        let event = ServerEvent::TransactionValidated {
            transaction: serde_json::json!({"Account": "rTest"}),
            meta: serde_json::json!({"TransactionResult": "tesSUCCESS"}),
            ledger_index: 10,
        };
        let json = event_to_json(&event);
        assert_eq!(json["type"], "transaction");
        assert_eq!(json["validated"], true);
        assert_eq!(json["status"], "closed");
        assert!(json.get("id").is_none());
    }

    #[test]
    fn transaction_proposed_event_json() {
        let event = ServerEvent::TransactionProposed {
            transaction: serde_json::json!({"Account": "rTest"}),
            engine_result: "tesSUCCESS".into(),
            engine_result_code: 0,
        };
        let json = event_to_json(&event);
        assert_eq!(json["type"], "transaction");
        assert_eq!(json["validated"], false);
        assert_eq!(json["status"], "proposed");
    }
}
