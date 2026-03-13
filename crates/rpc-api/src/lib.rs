//! XRPL JSON-RPC API type definitions.
//!
//! Provides request/response types, method enums, and error codes
//! for the XRPL JSON-RPC and WebSocket APIs.

pub mod error;
pub mod method;
pub mod requests;
pub mod responses;
pub mod types;
pub mod version;

pub use error::RpcErrorCode;
pub use method::Method;
pub use types::{JsonRpcRequest, JsonRpcResponse, RpcError};
pub use version::ApiVersion;
