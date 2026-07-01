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

// rippled per-field metadata flags (`SField::kSmd*`). A field appears in a
// metadata section only when its flags intersect that section's mask.
const SMD_CHANGE_ORIG: u32 = 0x01; // previous value when it changes
const SMD_CHANGE_NEW: u32 = 0x02; // new value when it changes
const SMD_DELETE_FINAL: u32 = 0x04; // final value when it is deleted
const SMD_CREATE: u32 = 0x08; // value when it is created
const SMD_ALWAYS: u32 = 0x10; // value whenever the node is affected
const SMD_DEFAULT: u32 = SMD_CHANGE_ORIG | SMD_CHANGE_NEW | SMD_DELETE_FINAL | SMD_CREATE;

/// rippled's metadata flags for `field`. Only fields whose flags differ from
/// `kSmdDefault` are listed (`include/xrpl/protocol/detail/sfields.macro`);
/// everything else takes the default and appears in every section.
fn smd_flags(field: &str) -> u32 {
    match field {
        "Indexes" | "LedgerEntryType" => 0,
        "PreviousTxnID" | "PreviousTxnLgrSeq" => SMD_DELETE_FINAL,
        "RootIndex" => SMD_ALWAYS,
        _ => SMD_DEFAULT,
    }
}

fn should_meta(field: &str, mask: u32) -> bool {
    smd_flags(field) & mask != 0
}

/// True when `v` equals its field type's default value, mirroring rippled's
/// `STBase::isDefault()`. Default fields are omitted from `NewFields`.
fn is_default_json(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => !b,
        serde_json::Value::Number(n) => n.as_u64() == Some(0) || n.as_i64() == Some(0),
        serde_json::Value::String(s) => s.is_empty() || s.chars().all(|c| c == '0'),
        serde_json::Value::Array(a) => a.is_empty(),
        // STAmount: default is a zero value (XRP or IOU).
        serde_json::Value::Object(o) => o.get("value").and_then(|x| x.as_str()) == Some("0"),
    }
}

fn decode_sle(bytes: &Option<Vec<u8>>) -> serde_json::Map<String, serde_json::Value> {
    bytes
        .as_ref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// Fields of `sle` kept for a metadata section, per rippled's `shouldMeta`.
/// `creating` additionally drops default-valued fields (the `NewFields` rule).
fn section_fields(
    sle: &serde_json::Map<String, serde_json::Value>,
    mask: u32,
    creating: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (k, v) in sle {
        if should_meta(k, mask) && !(creating && is_default_json(v)) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// `PreviousFields`: original values of fields that changed and carry the
/// `ChangeOrig` flag (rippled's `shouldMeta(kSmdChangeOrig)` over the orig node).
fn changed_previous_fields(
    prev: &serde_json::Map<String, serde_json::Value>,
    fin: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (k, pv) in prev {
        if should_meta(k, SMD_CHANGE_ORIG) && fin.get(k) != Some(pv) {
            out.insert(k.clone(), pv.clone());
        }
    }
    out
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
                previous: self
                    .originals
                    .get(key)
                    .cloned()
                    .or_else(|| Some(data.clone())),
                // The deleted entry's FINAL state (e.g. an offer consumed to
                // zero before deletion). Equals the original when nothing
                // mutated it first, so a plain delete shows no PreviousFields.
                final_fields: Some(data.clone()),
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

impl TxMeta {
    /// Serialize to rippled's canonical transaction-metadata JSON shape, ready
    /// for binary encoding. Affected nodes are sorted by ledger index (rippled
    /// convention); each carries the node-level LedgerEntryType / LedgerIndex /
    /// PreviousTxnID(LgrSeq) plus FinalFields / PreviousFields / NewFields.
    pub fn to_canonical_json(&self) -> serde_json::Value {
        let mut nodes = self.affected_nodes.clone();
        nodes.sort_by(|a, b| a.key.as_bytes().cmp(b.key.as_bytes()));

        let mut affected: Vec<serde_json::Value> = Vec::with_capacity(nodes.len());
        for n in &nodes {
            let prev = decode_sle(&n.previous);
            let fin = decode_sle(&n.final_fields);

            let mut inner = serde_json::Map::new();
            inner.insert("LedgerEntryType".into(), n.ledger_entry_type.clone().into());
            inner.insert(
                "LedgerIndex".into(),
                hex::encode_upper(n.key.as_bytes()).into(),
            );

            let wrapper = match n.change_type {
                ChangeType::Created => {
                    let news = section_fields(&fin, SMD_CREATE | SMD_ALWAYS, true);
                    if !news.is_empty() {
                        inner.insert("NewFields".into(), news.into());
                    }
                    "CreatedNode"
                }
                ChangeType::Modified => {
                    // rippled skips a modified node whose final state equals the original.
                    if prev == fin {
                        continue;
                    }
                    // Threaded types (those carrying PreviousTxnID) record the
                    // PREVIOUS values at the node level, via threadItem.
                    for f in ["PreviousTxnID", "PreviousTxnLgrSeq"] {
                        if let Some(v) = prev.get(f).filter(|v| !is_default_json(v)) {
                            inner.insert(f.into(), v.clone());
                        }
                    }
                    // When the entry's only delta is the PreviousTxnID threading
                    // (e.g. an Escrow/PayChannel destination AccountRoot that gains
                    // a directory link but no field change), rippled emits the
                    // node header alone — no FinalFields, no PreviousFields. Only
                    // a real field change (add / remove / modify, ignoring the
                    // threading bookkeeping) populates those sections.
                    let strip_thread = |m: &serde_json::Map<String, serde_json::Value>| {
                        let mut c = m.clone();
                        c.remove("PreviousTxnID");
                        c.remove("PreviousTxnLgrSeq");
                        c
                    };
                    if strip_thread(&prev) != strip_thread(&fin) {
                        let finals = section_fields(&fin, SMD_ALWAYS | SMD_CHANGE_NEW, false);
                        if !finals.is_empty() {
                            inner.insert("FinalFields".into(), finals.into());
                        }
                        let prevs = changed_previous_fields(&prev, &fin);
                        if !prevs.is_empty() {
                            inner.insert("PreviousFields".into(), prevs.into());
                        }
                    }
                    "ModifiedNode"
                }
                ChangeType::Deleted => {
                    let finals = section_fields(&fin, SMD_ALWAYS | SMD_DELETE_FINAL, false);
                    if !finals.is_empty() {
                        inner.insert("FinalFields".into(), finals.into());
                    }
                    let prevs = changed_previous_fields(&prev, &fin);
                    if !prevs.is_empty() {
                        inner.insert("PreviousFields".into(), prevs.into());
                    }
                    "DeletedNode"
                }
            };
            affected.push(serde_json::json!({ wrapper: serde_json::Value::Object(inner) }));
        }

        let mut meta = serde_json::Map::new();
        meta.insert("TransactionIndex".into(), self.tx_index.into());
        meta.insert("TransactionResult".into(), self.result_code.into());
        meta.insert("AffectedNodes".into(), serde_json::Value::Array(affected));
        if let Some(amt) = &self.delivered_amount {
            meta.insert("DeliveredAmount".into(), amt.clone().into());
        }
        serde_json::Value::Object(meta)
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
        // The deleted node carries its final state (here equal to the original,
        // since nothing mutated it before deletion).
        assert!(meta.affected_nodes[0].final_fields.is_some());
    }
}
