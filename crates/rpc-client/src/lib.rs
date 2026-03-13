//! Async XRPL RPC client supporting HTTP and WebSocket transports.
//!
//! Provides `XrplClient` with convenience methods for all standard XRPL
//! RPC commands, typed responses, and WebSocket subscription support.

pub mod builder;
pub mod client;
pub mod error;
pub mod http;
pub mod subscription;
pub mod transport;
pub mod websocket;

pub use builder::ClientBuilder;
pub use client::XrplClient;
pub use error::ClientError;
