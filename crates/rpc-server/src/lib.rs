/// XRPL JSON-RPC and WebSocket server.
///
/// Provides:
/// - HTTP JSON-RPC endpoint
/// - WebSocket endpoint with subscriptions
/// - Method dispatch router
/// - Server info, fee, and ping handlers
pub mod context;
pub mod error;
pub mod events;
pub mod handlers;
pub mod metrics;
pub mod rate_limit;
pub mod role;
pub mod router;
pub mod server;
pub mod subscriptions;

pub use context::{PrunerState, ServerContext};
pub use error::RpcServerError;
pub use events::ServerEvent;
pub use server::build_router;
