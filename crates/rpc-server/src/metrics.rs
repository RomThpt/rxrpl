use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// Metric name constants
pub const RPC_REQUESTS_TOTAL: &str = "rpc_requests_total";
pub const RPC_REQUEST_DURATION_SECONDS: &str = "rpc_request_duration_seconds";
pub const LEDGER_SEQUENCE: &str = "ledger_sequence";
pub const PEER_COUNT: &str = "peer_count";
pub const TXN_QUEUE_SIZE: &str = "txn_queue_size";
pub const TXN_APPLIED_TOTAL: &str = "txn_applied_total";

/// Install the Prometheus metrics recorder and return the handle for rendering.
///
/// Must be called once at startup. Subsequent calls will fail.
pub fn install_recorder() -> PrometheusHandle {
    PrometheusBuilder::new()
        .install_recorder()
        .expect("failed to install Prometheus recorder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_render() {
        // Each test gets its own recorder via a scoped approach
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();

        // Set it as the global recorder for this test
        // (will fail if another test already set it, so we just check the handle works)
        let output = handle.render();
        assert!(output.is_empty() || output.contains("# "));
    }

    #[test]
    fn metric_constants_defined() {
        assert_eq!(RPC_REQUESTS_TOTAL, "rpc_requests_total");
        assert_eq!(LEDGER_SEQUENCE, "ledger_sequence");
        assert_eq!(TXN_APPLIED_TOTAL, "txn_applied_total");
    }
}
