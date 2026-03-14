use std::sync::Arc;

use rxrpl_config::ServerConfig;

/// Shared state for all RPC handlers.
pub struct ServerContext {
    pub config: ServerConfig,
    // Future: Add ledger cache, tx queue, overlay reference, etc.
}

impl ServerContext {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        Arc::new(Self { config })
    }
}
