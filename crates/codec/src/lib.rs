//! XRPL binary codec and address encoding.
//!
//! Handles serialization/deserialization of XRPL transactions to/from binary format,
//! classic address encoding (rXXX), X-address encoding (XLS-5d), and seed encoding.

pub mod address;
pub mod binary;
pub mod error;

pub use error::CodecError;
