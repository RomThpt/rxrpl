/// XRPL SHAMap merkle tree implementation.
///
/// A SHAMap is a 16-way radix trie keyed by 256-bit hashes, where each
/// node is hashed using SHA-512-Half. The root hash provides a deterministic
/// fingerprint of the entire tree state, used for consensus.
pub mod error;
pub mod node;
pub mod node_id;
pub mod tree;

pub use error::SHAMapError;
pub use node::{LeafType, TreeNode};
pub use tree::{SHAMap, SHAMapState};
