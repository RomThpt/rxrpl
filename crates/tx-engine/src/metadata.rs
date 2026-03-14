use rxrpl_primitives::Hash256;

/// Transaction metadata tracking which ledger entries were affected.
#[derive(Clone, Debug, Default)]
pub struct TxMeta {
    /// Nodes that were created, modified, or deleted.
    pub affected_nodes: Vec<AffectedNode>,
    /// Index of this transaction within the ledger.
    pub tx_index: u32,
    /// Result code.
    pub result_code: i32,
    /// Delivered amount for payments (if applicable).
    pub delivered_amount: Option<String>,
}

/// A ledger entry affected by a transaction.
#[derive(Clone, Debug)]
pub struct AffectedNode {
    /// The type of change.
    pub change_type: ChangeType,
    /// The ledger entry key.
    pub key: Hash256,
    /// The ledger entry type name.
    pub ledger_entry_type: String,
    /// Previous state (for modified/deleted).
    pub previous: Option<Vec<u8>>,
    /// New state (for created/modified).
    pub final_fields: Option<Vec<u8>>,
}

/// How a ledger entry was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}
