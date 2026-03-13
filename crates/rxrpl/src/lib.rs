//! # rxrpl -- XRPL SDK for Rust
//!
//! A modular, type-safe Rust SDK for the XRP Ledger. This facade crate
//! re-exports the individual workspace crates under a single dependency,
//! controlled by feature flags.
//!
//! ## Feature flags
//!
//! | Feature    | Enables                              | Default |
//! |------------|--------------------------------------|---------|
//! | `crypto`   | Key generation, signing primitives   | yes     |
//! | `codec`    | Binary codec, address encoding       | yes     |
//! | `protocol` | Transactions, wallet, signing        | yes     |
//! | `rpc`      | RPC API type definitions             | no      |
//! | `client`   | Async RPC client (HTTP + WebSocket)  | no      |
//! | `autofill` | Transaction autofill via RPC         | no      |
//! | `full`     | Everything above                     | no      |
//!
//! ## Quick start
//!
//! ```toml
//! [dependencies]
//! rxrpl = "0.1"
//! ```
//!
//! ```rust,no_run
//! use rxrpl::{Wallet, KeyType};
//!
//! let wallet = Wallet::generate(KeyType::Ed25519);
//! println!("Address: {}", wallet.address);
//! ```

// -- Always available: primitives --
pub use rxrpl_primitives as primitives;

pub use rxrpl_primitives::{
    AccountId, Amount, CurrencyCode, Hash256, IssuedAmount, Issue, LedgerIndex, PublicKey,
    Signature, XrpAmount,
};

// -- crypto feature --
#[cfg(feature = "crypto")]
pub use rxrpl_crypto as crypto;

#[cfg(feature = "crypto")]
pub use rxrpl_crypto::{CryptoError, KeyPair, KeyType, Seed};

// -- codec feature --
#[cfg(feature = "codec")]
pub use rxrpl_codec as codec;

#[cfg(feature = "codec")]
pub use rxrpl_codec::CodecError;

// -- protocol feature --
#[cfg(feature = "protocol")]
pub use rxrpl_protocol as protocol;

#[cfg(feature = "protocol")]
pub use rxrpl_protocol::{
    LedgerEntryType, ProtocolError, TransactionResult, TransactionType, Wallet,
};

// -- rpc feature --
#[cfg(feature = "rpc")]
pub use rxrpl_rpc_api as rpc_api;

#[cfg(feature = "rpc")]
pub use rxrpl_rpc_api::{ApiVersion, JsonRpcRequest, JsonRpcResponse, Method, RpcError, RpcErrorCode};

// -- client feature --
#[cfg(feature = "client")]
pub use rxrpl_rpc_client as rpc_client;

#[cfg(feature = "client")]
pub use rxrpl_rpc_client::{ClientBuilder, ClientError, XrplClient};

/// Prelude module with the most commonly used types.
///
/// ```rust
/// use rxrpl::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{AccountId, Amount, Hash256, PublicKey};

    #[cfg(feature = "crypto")]
    pub use crate::{KeyPair, KeyType, Seed};

    #[cfg(feature = "protocol")]
    pub use crate::Wallet;

    #[cfg(feature = "client")]
    pub use crate::{ClientBuilder, XrplClient};
}
