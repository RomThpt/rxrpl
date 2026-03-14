/// XRPL JSON-RPC and WebSocket server.
///
/// Provides:
/// - HTTP JSON-RPC endpoint
/// - Method dispatch router
/// - Server info, fee, and ping handlers
pub mod context;
pub mod error;
pub mod handlers;
pub mod router;
pub mod server;

pub use context::ServerContext;
pub use error::RpcServerError;
pub use server::build_router;
