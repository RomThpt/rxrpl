use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// ---------------------------------------------------------------------------
// RPC metrics
// ---------------------------------------------------------------------------

/// Total RPC requests processed (labels: method).
pub const RPC_REQUESTS_TOTAL: &str = "rpc_requests_total";
/// RPC request duration in seconds (labels: method).
pub const RPC_REQUEST_DURATION_SECONDS: &str = "rpc_request_duration_seconds";
/// Total RPC errors (labels: method, error_type).
pub const RPC_ERRORS_TOTAL: &str = "rpc_errors_total";

// ---------------------------------------------------------------------------
// Consensus metrics
// ---------------------------------------------------------------------------

/// Total consensus rounds completed.
pub const CONSENSUS_ROUNDS_TOTAL: &str = "consensus_rounds_total";
/// Duration of the last consensus round in seconds.
pub const CONSENSUS_ROUND_DURATION_SECONDS: &str = "consensus_round_duration_seconds";
/// Total proposals received from peers.
pub const CONSENSUS_PROPOSALS_RECEIVED_TOTAL: &str = "consensus_proposals_received_total";
/// Total proposals sent to peers.
pub const CONSENSUS_PROPOSALS_SENT_TOTAL: &str = "consensus_proposals_sent_total";
/// Total validations received.
pub const CONSENSUS_VALIDATIONS_RECEIVED_TOTAL: &str = "consensus_validations_received_total";
/// Total validations sent.
pub const CONSENSUS_VALIDATIONS_SENT_TOTAL: &str = "consensus_validations_sent_total";
/// Total number of consensus stalls (exceeded max rounds).
pub const CONSENSUS_STALLS_TOTAL: &str = "consensus_stalls_total";

// ---------------------------------------------------------------------------
// P2P / overlay metrics
// ---------------------------------------------------------------------------

/// Current number of connected peers.
pub const P2P_PEERS_CONNECTED: &str = "p2p_peers_connected";
/// Total P2P messages sent (labels: msg_type).
pub const P2P_MESSAGES_SENT_TOTAL: &str = "p2p_messages_sent_total";
/// Total P2P messages received (labels: msg_type).
pub const P2P_MESSAGES_RECEIVED_TOTAL: &str = "p2p_messages_received_total";
/// Total bytes sent to peers.
pub const P2P_BYTES_SENT_TOTAL: &str = "p2p_bytes_sent_total";
/// Total bytes received from peers.
pub const P2P_BYTES_RECEIVED_TOTAL: &str = "p2p_bytes_received_total";
/// Total peer disconnections.
pub const P2P_DISCONNECTS_TOTAL: &str = "p2p_disconnects_total";
/// Distribution of peer scores (0-100 normalized score).
pub const P2P_PEER_SCORE: &str = "p2p_peer_score";

// ---------------------------------------------------------------------------
// Transaction queue metrics
// ---------------------------------------------------------------------------

/// Current transaction queue size.
pub const TXQ_SIZE: &str = "txq_size";
/// Total transactions queued.
pub const TXQ_QUEUED_TOTAL: &str = "txq_queued_total";
/// Total transactions dequeued (applied or expired).
pub const TXQ_DEQUEUED_TOTAL: &str = "txq_dequeued_total";
/// Total transactions rejected due to queue full.
pub const TXQ_REJECTED_FULL_TOTAL: &str = "txq_rejected_full_total";
/// Total transactions rejected due to account queue full.
pub const TXQ_REJECTED_ACCOUNT_FULL_TOTAL: &str = "txq_rejected_account_full_total";
/// Total duplicate transaction submissions.
pub const TXQ_REJECTED_DUPLICATE_TOTAL: &str = "txq_rejected_duplicate_total";
/// Current escalated fee in drops.
pub const TXQ_ESCALATED_FEE_DROPS: &str = "txq_escalated_fee_drops";

// ---------------------------------------------------------------------------
// Ledger metrics
// ---------------------------------------------------------------------------

/// Current ledger sequence number.
pub const LEDGER_SEQUENCE: &str = "ledger_sequence";
/// Duration of the last ledger close in seconds.
pub const LEDGER_CLOSE_DURATION_SECONDS: &str = "ledger_close_duration_seconds";
/// Number of transactions in the last closed ledger.
pub const LEDGER_TX_COUNT: &str = "ledger_tx_count";
/// Total transactions applied across all ledgers.
pub const LEDGER_TXN_APPLIED_TOTAL: &str = "ledger_txn_applied_total";

// ---------------------------------------------------------------------------
// SHAMap metrics
// ---------------------------------------------------------------------------

/// Total SHAMap node store cache hits.
pub const SHAMAP_CACHE_HITS_TOTAL: &str = "shamap_cache_hits_total";
/// Total SHAMap node store cache misses.
pub const SHAMAP_CACHE_MISSES_TOTAL: &str = "shamap_cache_misses_total";
/// Total lazy-loaded nodes from the backing store.
pub const SHAMAP_LAZY_LOAD_TOTAL: &str = "shamap_lazy_load_total";
/// Duration of SHAMap flush operations in seconds.
pub const SHAMAP_FLUSH_DURATION_SECONDS: &str = "shamap_flush_duration_seconds";

// ---------------------------------------------------------------------------
// NodeStore metrics
// ---------------------------------------------------------------------------

/// Total node store read operations.
pub const NODESTORE_READS_TOTAL: &str = "nodestore_reads_total";
/// Total node store write operations.
pub const NODESTORE_WRITES_TOTAL: &str = "nodestore_writes_total";
/// Total node store cache hits (positive cache).
pub const NODESTORE_CACHE_HITS_TOTAL: &str = "nodestore_cache_hits_total";
/// Total node store cache misses.
pub const NODESTORE_CACHE_MISSES_TOTAL: &str = "nodestore_cache_misses_total";
/// Total node store negative cache hits.
pub const NODESTORE_NEGATIVE_CACHE_HITS_TOTAL: &str = "nodestore_negative_cache_hits_total";

// ---------------------------------------------------------------------------
// Legacy aliases (kept for backward compatibility)
// ---------------------------------------------------------------------------

pub const PEER_COUNT: &str = P2P_PEERS_CONNECTED;
pub const TXN_QUEUE_SIZE: &str = TXQ_SIZE;
pub const TXN_APPLIED_TOTAL: &str = LEDGER_TXN_APPLIED_TOTAL;

// ---------------------------------------------------------------------------
// Convenience helpers for recording metrics
// ---------------------------------------------------------------------------

/// Record an RPC error, incrementing the error counter with labels.
pub fn record_rpc_error(method: &str, error_type: &str) {
    ::metrics::counter!(
        RPC_ERRORS_TOTAL,
        "method" => method.to_string(),
        "error_type" => error_type.to_string(),
    )
    .increment(1);
}

/// Record a P2P message sent with its type.
pub fn record_p2p_message_sent(msg_type: &str, bytes: u64) {
    ::metrics::counter!(
        P2P_MESSAGES_SENT_TOTAL,
        "msg_type" => msg_type.to_string(),
    )
    .increment(1);
    ::metrics::counter!(P2P_BYTES_SENT_TOTAL).increment(bytes);
}

/// Record a P2P message received with its type.
pub fn record_p2p_message_received(msg_type: &str, bytes: u64) {
    ::metrics::counter!(
        P2P_MESSAGES_RECEIVED_TOTAL,
        "msg_type" => msg_type.to_string(),
    )
    .increment(1);
    ::metrics::counter!(P2P_BYTES_RECEIVED_TOTAL).increment(bytes);
}

/// Record a peer disconnection.
pub fn record_peer_disconnect() {
    ::metrics::counter!(P2P_DISCONNECTS_TOTAL).increment(1);
}

/// Update the connected peers gauge.
pub fn set_peer_count(count: f64) {
    ::metrics::gauge!(P2P_PEERS_CONNECTED).set(count);
}

/// Record a peer score observation for the score distribution histogram.
pub fn record_peer_score(score: f64) {
    ::metrics::histogram!(P2P_PEER_SCORE).record(score);
}

/// Record a transaction queued successfully.
pub fn record_tx_queued() {
    ::metrics::counter!(TXQ_QUEUED_TOTAL).increment(1);
}

/// Record a transaction dequeued (applied or expired).
pub fn record_tx_dequeued() {
    ::metrics::counter!(TXQ_DEQUEUED_TOTAL).increment(1);
}

/// Record a transaction rejected because the queue was full.
pub fn record_tx_rejected_full() {
    ::metrics::counter!(TXQ_REJECTED_FULL_TOTAL).increment(1);
}

/// Record a transaction rejected because the per-account queue was full.
pub fn record_tx_rejected_account_full() {
    ::metrics::counter!(TXQ_REJECTED_ACCOUNT_FULL_TOTAL).increment(1);
}

/// Record a duplicate transaction submission.
pub fn record_tx_rejected_duplicate() {
    ::metrics::counter!(TXQ_REJECTED_DUPLICATE_TOTAL).increment(1);
}

/// Update the transaction queue size gauge.
pub fn set_txq_size(size: f64) {
    ::metrics::gauge!(TXQ_SIZE).set(size);
}

/// Update the escalated fee gauge.
pub fn set_escalated_fee(drops: f64) {
    ::metrics::gauge!(TXQ_ESCALATED_FEE_DROPS).set(drops);
}

/// Update the current ledger sequence gauge.
pub fn set_ledger_sequence(seq: f64) {
    ::metrics::gauge!(LEDGER_SEQUENCE).set(seq);
}

/// Record a ledger close duration.
pub fn record_ledger_close_duration(seconds: f64) {
    ::metrics::histogram!(LEDGER_CLOSE_DURATION_SECONDS).record(seconds);
}

/// Set the transaction count of the last closed ledger.
pub fn set_ledger_tx_count(count: f64) {
    ::metrics::gauge!(LEDGER_TX_COUNT).set(count);
}

/// Record a consensus round completion.
pub fn record_consensus_round(duration_seconds: f64) {
    ::metrics::counter!(CONSENSUS_ROUNDS_TOTAL).increment(1);
    ::metrics::histogram!(CONSENSUS_ROUND_DURATION_SECONDS).record(duration_seconds);
}

/// Record a consensus stall.
pub fn record_consensus_stall() {
    ::metrics::counter!(CONSENSUS_STALLS_TOTAL).increment(1);
}

/// Record a proposal sent.
pub fn record_proposal_sent() {
    ::metrics::counter!(CONSENSUS_PROPOSALS_SENT_TOTAL).increment(1);
}

/// Record a proposal received.
pub fn record_proposal_received() {
    ::metrics::counter!(CONSENSUS_PROPOSALS_RECEIVED_TOTAL).increment(1);
}

/// Record a validation sent.
pub fn record_validation_sent() {
    ::metrics::counter!(CONSENSUS_VALIDATIONS_SENT_TOTAL).increment(1);
}

/// Record a validation received.
pub fn record_validation_received() {
    ::metrics::counter!(CONSENSUS_VALIDATIONS_RECEIVED_TOTAL).increment(1);
}

/// Record a SHAMap cache hit.
pub fn record_shamap_cache_hit() {
    ::metrics::counter!(SHAMAP_CACHE_HITS_TOTAL).increment(1);
}

/// Record a SHAMap cache miss.
pub fn record_shamap_cache_miss() {
    ::metrics::counter!(SHAMAP_CACHE_MISSES_TOTAL).increment(1);
}

/// Record a SHAMap lazy load from the backing store.
pub fn record_shamap_lazy_load() {
    ::metrics::counter!(SHAMAP_LAZY_LOAD_TOTAL).increment(1);
}

/// Record a SHAMap flush duration.
pub fn record_shamap_flush_duration(seconds: f64) {
    ::metrics::histogram!(SHAMAP_FLUSH_DURATION_SECONDS).record(seconds);
}

/// Record a node store read operation.
pub fn record_nodestore_read() {
    ::metrics::counter!(NODESTORE_READS_TOTAL).increment(1);
}

/// Record a node store write operation.
pub fn record_nodestore_write() {
    ::metrics::counter!(NODESTORE_WRITES_TOTAL).increment(1);
}

/// Record a node store positive cache hit.
pub fn record_nodestore_cache_hit() {
    ::metrics::counter!(NODESTORE_CACHE_HITS_TOTAL).increment(1);
}

/// Record a node store cache miss.
pub fn record_nodestore_cache_miss() {
    ::metrics::counter!(NODESTORE_CACHE_MISSES_TOTAL).increment(1);
}

/// Record a node store negative cache hit.
pub fn record_nodestore_negative_cache_hit() {
    ::metrics::counter!(NODESTORE_NEGATIVE_CACHE_HITS_TOTAL).increment(1);
}

/// Install the Prometheus metrics recorder and return the handle for rendering.
///
/// Must be called once at startup. Subsequent calls will fail.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

/// Describe all metrics with help text for the Prometheus exporter.
///
/// Should be called once after the recorder is installed. This populates
/// the `# HELP` and `# TYPE` annotations in the Prometheus output.
pub fn describe_metrics() {
    use metrics::{describe_counter, describe_gauge, describe_histogram};

    // RPC
    describe_counter!(RPC_REQUESTS_TOTAL, "Total RPC requests processed");
    describe_histogram!(
        RPC_REQUEST_DURATION_SECONDS,
        "RPC request duration in seconds"
    );
    describe_counter!(RPC_ERRORS_TOTAL, "Total RPC errors by method and type");

    // Consensus
    describe_counter!(
        CONSENSUS_ROUNDS_TOTAL,
        "Total consensus rounds completed"
    );
    describe_histogram!(
        CONSENSUS_ROUND_DURATION_SECONDS,
        "Consensus round duration in seconds"
    );
    describe_counter!(
        CONSENSUS_PROPOSALS_RECEIVED_TOTAL,
        "Total proposals received from peers"
    );
    describe_counter!(
        CONSENSUS_PROPOSALS_SENT_TOTAL,
        "Total proposals sent to peers"
    );
    describe_counter!(
        CONSENSUS_VALIDATIONS_RECEIVED_TOTAL,
        "Total validations received"
    );
    describe_counter!(
        CONSENSUS_VALIDATIONS_SENT_TOTAL,
        "Total validations sent"
    );
    describe_counter!(
        CONSENSUS_STALLS_TOTAL,
        "Total consensus stalls (max rounds exceeded)"
    );

    // P2P
    describe_gauge!(P2P_PEERS_CONNECTED, "Current number of connected peers");
    describe_counter!(
        P2P_MESSAGES_SENT_TOTAL,
        "Total P2P messages sent by type"
    );
    describe_counter!(
        P2P_MESSAGES_RECEIVED_TOTAL,
        "Total P2P messages received by type"
    );
    describe_counter!(P2P_BYTES_SENT_TOTAL, "Total bytes sent to peers");
    describe_counter!(
        P2P_BYTES_RECEIVED_TOTAL,
        "Total bytes received from peers"
    );
    describe_counter!(P2P_DISCONNECTS_TOTAL, "Total peer disconnections");
    describe_histogram!(
        P2P_PEER_SCORE,
        "Distribution of peer normalized scores (0-100)"
    );

    // TxQueue
    describe_gauge!(TXQ_SIZE, "Current transaction queue size");
    describe_counter!(TXQ_QUEUED_TOTAL, "Total transactions queued");
    describe_counter!(
        TXQ_DEQUEUED_TOTAL,
        "Total transactions dequeued (applied/expired)"
    );
    describe_counter!(
        TXQ_REJECTED_FULL_TOTAL,
        "Total transactions rejected (queue full)"
    );
    describe_counter!(
        TXQ_REJECTED_ACCOUNT_FULL_TOTAL,
        "Total transactions rejected (per-account limit)"
    );
    describe_counter!(
        TXQ_REJECTED_DUPLICATE_TOTAL,
        "Total duplicate transaction submissions"
    );
    describe_gauge!(
        TXQ_ESCALATED_FEE_DROPS,
        "Current escalated fee in drops"
    );

    // Ledger
    describe_gauge!(LEDGER_SEQUENCE, "Current ledger sequence number");
    describe_histogram!(
        LEDGER_CLOSE_DURATION_SECONDS,
        "Ledger close duration in seconds"
    );
    describe_gauge!(
        LEDGER_TX_COUNT,
        "Number of transactions in the last closed ledger"
    );
    describe_counter!(
        LEDGER_TXN_APPLIED_TOTAL,
        "Total transactions applied across all ledgers"
    );

    // SHAMap
    describe_counter!(SHAMAP_CACHE_HITS_TOTAL, "Total SHAMap cache hits");
    describe_counter!(SHAMAP_CACHE_MISSES_TOTAL, "Total SHAMap cache misses");
    describe_counter!(
        SHAMAP_LAZY_LOAD_TOTAL,
        "Total SHAMap nodes lazy-loaded from store"
    );
    describe_histogram!(
        SHAMAP_FLUSH_DURATION_SECONDS,
        "SHAMap flush duration in seconds"
    );

    // NodeStore
    describe_counter!(NODESTORE_READS_TOTAL, "Total node store read operations");
    describe_counter!(
        NODESTORE_WRITES_TOTAL,
        "Total node store write operations"
    );
    describe_counter!(
        NODESTORE_CACHE_HITS_TOTAL,
        "Total node store positive cache hits"
    );
    describe_counter!(
        NODESTORE_CACHE_MISSES_TOTAL,
        "Total node store cache misses"
    );
    describe_counter!(
        NODESTORE_NEGATIVE_CACHE_HITS_TOTAL,
        "Total node store negative cache hits"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_render() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        let output = handle.render();
        assert!(output.is_empty() || output.contains("# "));
    }

    #[test]
    fn metric_constants_defined() {
        assert_eq!(RPC_REQUESTS_TOTAL, "rpc_requests_total");
        assert_eq!(RPC_ERRORS_TOTAL, "rpc_errors_total");
        assert_eq!(LEDGER_SEQUENCE, "ledger_sequence");
        assert_eq!(LEDGER_TXN_APPLIED_TOTAL, "ledger_txn_applied_total");
    }

    #[test]
    fn legacy_aliases_match() {
        assert_eq!(PEER_COUNT, P2P_PEERS_CONNECTED);
        assert_eq!(TXN_QUEUE_SIZE, TXQ_SIZE);
        assert_eq!(TXN_APPLIED_TOTAL, LEDGER_TXN_APPLIED_TOTAL);
    }

    #[test]
    fn all_metric_names_are_snake_case() {
        let all_names = [
            RPC_REQUESTS_TOTAL,
            RPC_REQUEST_DURATION_SECONDS,
            RPC_ERRORS_TOTAL,
            CONSENSUS_ROUNDS_TOTAL,
            CONSENSUS_ROUND_DURATION_SECONDS,
            CONSENSUS_PROPOSALS_RECEIVED_TOTAL,
            CONSENSUS_PROPOSALS_SENT_TOTAL,
            CONSENSUS_VALIDATIONS_RECEIVED_TOTAL,
            CONSENSUS_VALIDATIONS_SENT_TOTAL,
            CONSENSUS_STALLS_TOTAL,
            P2P_PEERS_CONNECTED,
            P2P_MESSAGES_SENT_TOTAL,
            P2P_MESSAGES_RECEIVED_TOTAL,
            P2P_BYTES_SENT_TOTAL,
            P2P_BYTES_RECEIVED_TOTAL,
            P2P_DISCONNECTS_TOTAL,
            P2P_PEER_SCORE,
            TXQ_SIZE,
            TXQ_QUEUED_TOTAL,
            TXQ_DEQUEUED_TOTAL,
            TXQ_REJECTED_FULL_TOTAL,
            TXQ_REJECTED_ACCOUNT_FULL_TOTAL,
            TXQ_REJECTED_DUPLICATE_TOTAL,
            TXQ_ESCALATED_FEE_DROPS,
            LEDGER_SEQUENCE,
            LEDGER_CLOSE_DURATION_SECONDS,
            LEDGER_TX_COUNT,
            LEDGER_TXN_APPLIED_TOTAL,
            SHAMAP_CACHE_HITS_TOTAL,
            SHAMAP_CACHE_MISSES_TOTAL,
            SHAMAP_LAZY_LOAD_TOTAL,
            SHAMAP_FLUSH_DURATION_SECONDS,
            NODESTORE_READS_TOTAL,
            NODESTORE_WRITES_TOTAL,
            NODESTORE_CACHE_HITS_TOTAL,
            NODESTORE_CACHE_MISSES_TOTAL,
            NODESTORE_NEGATIVE_CACHE_HITS_TOTAL,
        ];

        for name in all_names {
            assert!(
                !name.is_empty(),
                "metric name must not be empty"
            );
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "metric name must be snake_case: {name}"
            );
        }
    }

    #[test]
    fn no_duplicate_metric_names() {
        let all_names = [
            RPC_REQUESTS_TOTAL,
            RPC_REQUEST_DURATION_SECONDS,
            RPC_ERRORS_TOTAL,
            CONSENSUS_ROUNDS_TOTAL,
            CONSENSUS_ROUND_DURATION_SECONDS,
            CONSENSUS_PROPOSALS_RECEIVED_TOTAL,
            CONSENSUS_PROPOSALS_SENT_TOTAL,
            CONSENSUS_VALIDATIONS_RECEIVED_TOTAL,
            CONSENSUS_VALIDATIONS_SENT_TOTAL,
            CONSENSUS_STALLS_TOTAL,
            P2P_PEERS_CONNECTED,
            P2P_MESSAGES_SENT_TOTAL,
            P2P_MESSAGES_RECEIVED_TOTAL,
            P2P_BYTES_SENT_TOTAL,
            P2P_BYTES_RECEIVED_TOTAL,
            P2P_DISCONNECTS_TOTAL,
            P2P_PEER_SCORE,
            TXQ_SIZE,
            TXQ_QUEUED_TOTAL,
            TXQ_DEQUEUED_TOTAL,
            TXQ_REJECTED_FULL_TOTAL,
            TXQ_REJECTED_ACCOUNT_FULL_TOTAL,
            TXQ_REJECTED_DUPLICATE_TOTAL,
            TXQ_ESCALATED_FEE_DROPS,
            LEDGER_SEQUENCE,
            LEDGER_CLOSE_DURATION_SECONDS,
            LEDGER_TX_COUNT,
            LEDGER_TXN_APPLIED_TOTAL,
            SHAMAP_CACHE_HITS_TOTAL,
            SHAMAP_CACHE_MISSES_TOTAL,
            SHAMAP_LAZY_LOAD_TOTAL,
            SHAMAP_FLUSH_DURATION_SECONDS,
            NODESTORE_READS_TOTAL,
            NODESTORE_WRITES_TOTAL,
            NODESTORE_CACHE_HITS_TOTAL,
            NODESTORE_CACHE_MISSES_TOTAL,
            NODESTORE_NEGATIVE_CACHE_HITS_TOTAL,
        ];

        let mut seen = std::collections::HashSet::new();
        for name in all_names {
            assert!(
                seen.insert(name),
                "duplicate metric name: {name}"
            );
        }
    }

    #[test]
    fn describe_metrics_does_not_panic() {
        // Build a scoped recorder so describe calls go somewhere valid.
        let recorder = PrometheusBuilder::new().build_recorder();
        let _handle = recorder.handle();
        // describe_metrics() calls metrics::describe_* which may be no-ops
        // without a global recorder, but must not panic.
        describe_metrics();
    }

    #[test]
    fn helper_functions_callable_without_recorder() {
        // All helper functions use the metrics facade which is a no-op
        // when no recorder is installed. Verify they do not panic.
        record_rpc_error("test", "internal");
        record_p2p_message_sent("proposal", 128);
        record_p2p_message_received("validation", 256);
        record_peer_disconnect();
        set_peer_count(5.0);
        record_peer_score(75.0);
        record_tx_queued();
        record_tx_dequeued();
        record_tx_rejected_full();
        record_tx_rejected_account_full();
        record_tx_rejected_duplicate();
        set_txq_size(10.0);
        set_escalated_fee(12.0);
        set_ledger_sequence(100.0);
        record_ledger_close_duration(1.5);
        set_ledger_tx_count(42.0);
        record_consensus_round(2.0);
        record_consensus_stall();
        record_proposal_sent();
        record_proposal_received();
        record_validation_sent();
        record_validation_received();
        record_shamap_cache_hit();
        record_shamap_cache_miss();
        record_shamap_lazy_load();
        record_shamap_flush_duration(0.05);
        record_nodestore_read();
        record_nodestore_write();
        record_nodestore_cache_hit();
        record_nodestore_cache_miss();
        record_nodestore_negative_cache_hit();
    }
}
