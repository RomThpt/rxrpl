//! XRPL protocol types: transactions, ledger entries, wallet, and signing.
//!
//! Provides typed transaction construction, single-sig and multisig signing,
//! signature verification, and a high-level `Wallet` API.

pub mod error;
pub mod flags;
pub mod keylet;
pub mod ledger;
pub mod tx;
pub mod types;
pub mod wallet;

pub use error::ProtocolError;
pub use types::{LedgerEntryType, TransactionResult, TransactionType};
pub use wallet::Wallet;
