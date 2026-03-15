use rxrpl_primitives::Hash256;

use crate::view::sandbox::SandboxChanges;

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

/// Extract the LedgerEntryType from a JSON-serialized ledger entry.
fn extract_entry_type(data: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v.get("LedgerEntryType")?.as_str().map(String::from))
        .unwrap_or_else(|| "Unknown".into())
}

impl SandboxChanges {
    /// Build transaction metadata from these sandbox changes.
    pub fn build_metadata(&self, tx_index: u32, result_code: i32) -> TxMeta {
        let mut affected_nodes = Vec::new();

        for (key, data) in &self.inserts {
            affected_nodes.push(AffectedNode {
                change_type: ChangeType::Created,
                key: *key,
                ledger_entry_type: extract_entry_type(data),
                previous: None,
                final_fields: Some(data.clone()),
            });
        }

        for (key, data) in &self.updates {
            affected_nodes.push(AffectedNode {
                change_type: ChangeType::Modified,
                key: *key,
                ledger_entry_type: extract_entry_type(data),
                previous: self.originals.get(key).cloned(),
                final_fields: Some(data.clone()),
            });
        }

        for (key, data) in &self.deletes {
            affected_nodes.push(AffectedNode {
                change_type: ChangeType::Deleted,
                key: *key,
                ledger_entry_type: extract_entry_type(data),
                previous: self.originals.get(key).cloned().or_else(|| Some(data.clone())),
                final_fields: None,
            });
        }

        TxMeta {
            affected_nodes,
            tx_index,
            result_code,
            delivered_amount: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_account_root_bytes(balance: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": balance,
        }))
        .unwrap()
    }

    #[test]
    fn metadata_created_node() {
        let key = Hash256::new([0x01; 32]);
        let data = make_account_root_bytes("1000000");
        let changes = SandboxChanges {
            inserts: HashMap::from([(key, data.clone())]),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        };

        let meta = changes.build_metadata(0, 0);
        assert_eq!(meta.affected_nodes.len(), 1);
        assert_eq!(meta.affected_nodes[0].change_type, ChangeType::Created);
        assert_eq!(meta.affected_nodes[0].ledger_entry_type, "AccountRoot");
        assert!(meta.affected_nodes[0].previous.is_none());
        assert!(meta.affected_nodes[0].final_fields.is_some());
    }

    #[test]
    fn metadata_modified_node() {
        let key = Hash256::new([0x02; 32]);
        let original = make_account_root_bytes("2000000");
        let updated = make_account_root_bytes("1000000");
        let changes = SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::from([(key, updated)]),
            deletes: HashMap::new(),
            originals: HashMap::from([(key, original)]),
            destroyed_drops: 10,
        };

        let meta = changes.build_metadata(1, 0);
        assert_eq!(meta.affected_nodes.len(), 1);
        assert_eq!(meta.affected_nodes[0].change_type, ChangeType::Modified);
        assert!(meta.affected_nodes[0].previous.is_some());
        assert!(meta.affected_nodes[0].final_fields.is_some());
    }

    #[test]
    fn metadata_deleted_node() {
        let key = Hash256::new([0x03; 32]);
        let data = make_account_root_bytes("500");
        let changes = SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::from([(key, data.clone())]),
            originals: HashMap::from([(key, data)]),
            destroyed_drops: 0,
        };

        let meta = changes.build_metadata(0, 0);
        assert_eq!(meta.affected_nodes.len(), 1);
        assert_eq!(meta.affected_nodes[0].change_type, ChangeType::Deleted);
        assert!(meta.affected_nodes[0].previous.is_some());
        assert!(meta.affected_nodes[0].final_fields.is_none());
    }
}
