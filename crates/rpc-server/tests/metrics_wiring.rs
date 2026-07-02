//! Verifies the Prometheus wiring end to end: `global_handle` installs the
//! process recorder (once) and `render()` reflects emitted metrics.
//!
//! Before this, `ServerContext.metrics_handle` was always `None`, so `/metrics`
//! returned 404 and every emission helper was dead code. Runs as its own test
//! binary so the global recorder it installs never collides with the local
//! recorders used by the `metrics` unit tests.

#[test]
fn global_handle_installs_and_renders_emitted_metrics() {
    use rxrpl_rpc_server::metrics;

    let handle = metrics::global_handle();
    // Idempotent: a second call must not panic. Re-installing the global
    // recorder would — this is exactly what makes several nodes in one process
    // (e.g. the in-process cluster test) safe.
    let handle2 = metrics::global_handle();

    metrics::set_ledger_sequence(4_242_424.0);
    metrics::set_ledger_tx_count(17.0);

    let out = handle.render();
    assert!(!out.is_empty(), "render produced no output");
    assert!(
        out.contains("ledger_sequence"),
        "ledger_sequence gauge missing from render:\n{out}"
    );
    assert!(
        out.contains("4242424"),
        "emitted gauge value not reflected in render:\n{out}"
    );

    // The second handle renders the same global registry.
    assert!(handle2.render().contains("ledger_sequence"));
}
