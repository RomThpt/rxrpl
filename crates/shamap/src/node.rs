use rxrpl_primitives::Hash256;

use crate::inner_node::InnerNode;
use crate::leaf_node::LeafNode;

/// A SHAMap tree node -- either an inner (branch) node or a leaf node.
#[derive(Clone, Debug)]
pub enum SHAMapNode {
    Inner(Box<InnerNode>),
    Leaf(LeafNode),
}

impl SHAMapNode {
    /// Create an inner node variant.
    pub fn inner(node: InnerNode) -> Self {
        SHAMapNode::Inner(Box::new(node))
    }

    /// Get the hash of this node.
    pub fn hash(&self) -> Hash256 {
        match self {
            SHAMapNode::Inner(n) => n.hash(),
            SHAMapNode::Leaf(n) => n.hash(),
        }
    }
}

/// The type of SHAMap (determines leaf hashing strategy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SHAMapType {
    /// Transaction tree (uses TransactionNoMeta hashing).
    Transaction,
    /// State tree (uses AccountState hashing).
    State,
}

/// The state of a SHAMap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SHAMapState {
    /// Open for changes.
    Modifying,
    /// Frozen, no changes allowed.
    Immutable,
    /// Being synced from network.
    Syncing,
    /// Invalid / corrupt state.
    Invalid,
}

/// The type of a node in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    Inner,
    TransactionNoMeta,
    TransactionWithMeta,
    AccountState,
}
