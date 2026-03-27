#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz the RPC router with arbitrary JSON
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(s) {
            let method = json
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("ping");
            let params = json
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let ctx =
                rxrpl_rpc_server::ServerContext::new(rxrpl_config::ServerConfig::default());
            let req_ctx = rxrpl_rpc_server::role::RequestContext {
                role: rxrpl_rpc_server::role::ConnectionRole::Public,
                api_version: rxrpl_rpc_api::ApiVersion::default(),
            };
            rt.block_on(async {
                let _ = rxrpl_rpc_server::router::dispatch(method, params, &ctx, &req_ctx).await;
            });
        }
    }
});
