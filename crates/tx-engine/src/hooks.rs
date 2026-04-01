//! Hook execution integration for the transaction engine.
//!
//! After a transaction is successfully applied, this module checks whether
//! the destination account has hooks installed and, if so, executes them.

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_hooks::{HookContext, HookExecutionEngine, HookResult};
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::view::read_view::ReadView;

/// Result of running hooks for a transaction.
#[derive(Clone, Debug)]
pub struct HookExecutionResult {
    /// Whether any hook called rollback.
    pub rollback: bool,
    /// Collected emitted transactions from all hooks.
    pub emitted_txns: Vec<Vec<u8>>,
    /// Individual results per hook.
    pub results: Vec<HookResult>,
}

/// Execute hooks for the destination account of a transaction.
///
/// Looks up the `HookDefinition` ledger entry for the destination account.
/// If hooks are installed, each hook's WASM is executed with a `HookContext`
/// populated from the originating transaction.
///
/// Returns `None` if the destination has no hooks, or `Some(result)` with
/// the combined execution outcome.
pub fn execute_hooks_for_tx(
    tx: &Value,
    tx_hash: &Hash256,
    view: &dyn ReadView,
) -> Option<HookExecutionResult> {
    // Determine destination account
    let dest_str = tx.get("Destination").and_then(|v| v.as_str())?;
    let dest_id = decode_account_id(dest_str).ok()?;

    // Look up HookDefinition for destination
    let hook_def_key = keylet::hook_definition(&dest_id);
    let hook_def_bytes = view.read(&hook_def_key)?;
    let hook_def: Value = serde_json::from_slice(&hook_def_bytes).ok()?;

    let hooks = hook_def.get("Hooks").and_then(|v| v.as_array())?;
    if hooks.is_empty() {
        return None;
    }

    // Build context from the originating transaction
    let otxn_type = extract_tx_type_code(tx);
    let otxn_account = extract_account_bytes(tx);
    let otxn_amount = extract_amount_drops(tx);
    let otxn_blob = serde_json::to_vec(tx).unwrap_or_default();

    let engine = HookExecutionEngine::new();
    let all_emitted = Vec::new();
    let mut results = Vec::new();
    let mut rollback = false;

    for hook_entry in hooks {
        let create_code_hex = match hook_entry.get("CreateCode").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };

        let wasm_bytes = match hex::decode(create_code_hex) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };

        let mut ctx = HookContext::with_otxn(
            *tx_hash,
            dest_id.0,
            otxn_blob.clone(),
            otxn_type,
            otxn_account,
            otxn_amount,
        );

        // Read HookOn from hook entry
        if let Some(hook_on_val) = hook_entry.get("HookOn").and_then(|v| v.as_str()) {
            if let Ok(bytes) = hex::decode(hook_on_val) {
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    ctx.hook_on = Some(arr);
                }
            }
        }

        // Read HookGrants from hook entry
        if let Some(grants_arr) = hook_entry.get("HookGrants").and_then(|v| v.as_array()) {
            for grant in grants_arr {
                let g = grant.get("HookGrant").unwrap_or(grant);
                let authorize = g
                    .get("Authorize")
                    .and_then(|v| v.as_str())
                    .and_then(|s| decode_account_id(s).ok().map(|a| a.0));
                let hook_hash = g
                    .get("HookHash")
                    .and_then(|v| v.as_str())
                    .and_then(|s| {
                        hex::decode(s).ok().and_then(|b| {
                            if b.len() == 32 {
                                let mut a = [0u8; 32];
                                a.copy_from_slice(&b);
                                Some(a)
                            } else {
                                None
                            }
                        })
                    });
                ctx.grants.push(rxrpl_hooks::HookGrant {
                    authorize,
                    hook_hash,
                });
            }
        }

        // Populate otxn_fields from the transaction JSON
        populate_otxn_fields(&mut ctx, tx);

        match engine.execute(&wasm_bytes, ctx) {
            Ok(result) => {
                if let HookResult::Rollback(_) = &result {
                    rollback = true;
                }
                results.push(result);
            }
            Err(_) => {
                results.push(HookResult::Error("hook execution failed".into()));
            }
        }
    }

    // In a full implementation, emitted txns would be extracted from each
    // hook's context after execution. The current engine consumes the context,
    // so this would require returning it. For now, emitted_txns remains empty
    // and can be wired up when the engine is extended.
    let _ = &all_emitted;

    Some(HookExecutionResult {
        rollback,
        emitted_txns: all_emitted,
        results,
    })
}

/// Extract the transaction type code from a JSON transaction.
fn extract_tx_type_code(tx: &Value) -> u16 {
    tx.get("TransactionType")
        .and_then(|v| v.as_str())
        .and_then(|s| {
            rxrpl_protocol::TransactionType::from_name(s)
                .ok()
                .map(|t| t.code())
        })
        .unwrap_or(0)
}

/// Extract the 20-byte account ID from a JSON transaction.
fn extract_account_bytes(tx: &Value) -> [u8; 20] {
    tx.get("Account")
        .and_then(|v| v.as_str())
        .and_then(|s| decode_account_id(s).ok())
        .map(|id| id.0)
        .unwrap_or([0u8; 20])
}

/// Extract the amount in drops from a JSON transaction.
fn extract_amount_drops(tx: &Value) -> i64 {
    match tx.get("Amount") {
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Populate the otxn_fields map from a JSON transaction.
///
/// Uses a simple field ID mapping for common XRPL fields.
fn populate_otxn_fields(ctx: &mut HookContext, tx: &Value) {
    let obj = match tx.as_object() {
        Some(o) => o,
        None => return,
    };

    for (key, value) in obj {
        let field_id = field_name_to_id(key);
        if field_id > 0 {
            if let Ok(bytes) = serde_json::to_vec(value) {
                ctx.otxn_fields.insert(field_id, bytes);
            }
        }
    }
}

/// Map common XRPL field names to numeric field IDs.
///
/// These follow the XRPL serialization format field codes.
fn field_name_to_id(name: &str) -> u32 {
    match name {
        "TransactionType" => 2,
        "Flags" => 3,
        "Sequence" => 4,
        "Amount" => 1_001,
        "Fee" => 1_008,
        "Destination" => 3_003,
        "Account" => 3_001,
        "SigningPubKey" => 7_003,
        "TxnSignature" => 7_004,
        "DestinationTag" => 14,
        "SourceTag" => 3,
        "InvoiceID" => 17,
        "Memos" => 15_009,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;

    #[test]
    fn extract_tx_type_code_payment() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        });
        let code = extract_tx_type_code(&tx);
        // Payment is type code 0 in XRPL
        assert_eq!(code, 0);
    }

    #[test]
    fn extract_tx_type_code_account_set() {
        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
        });
        let code = extract_tx_type_code(&tx);
        assert_eq!(code, 3); // AccountSet = 3
    }

    #[test]
    fn extract_tx_type_code_missing() {
        let tx = serde_json::json!({});
        assert_eq!(extract_tx_type_code(&tx), 0);
    }

    #[test]
    fn extract_account_bytes_valid() {
        let tx = serde_json::json!({
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        });
        let bytes = extract_account_bytes(&tx);
        assert_ne!(bytes, [0u8; 20]);
    }

    #[test]
    fn extract_account_bytes_missing() {
        let tx = serde_json::json!({});
        assert_eq!(extract_account_bytes(&tx), [0u8; 20]);
    }

    #[test]
    fn extract_amount_drops_string() {
        let tx = serde_json::json!({
            "Amount": "5000000",
        });
        assert_eq!(extract_amount_drops(&tx), 5_000_000);
    }

    #[test]
    fn extract_amount_drops_number() {
        let tx = serde_json::json!({
            "Amount": 5000000,
        });
        assert_eq!(extract_amount_drops(&tx), 5_000_000);
    }

    #[test]
    fn extract_amount_drops_missing() {
        let tx = serde_json::json!({});
        assert_eq!(extract_amount_drops(&tx), 0);
    }

    #[test]
    fn extract_amount_drops_object_returns_zero() {
        let tx = serde_json::json!({
            "Amount": {
                "currency": "USD",
                "value": "100",
                "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            },
        });
        assert_eq!(extract_amount_drops(&tx), 0);
    }

    #[test]
    fn field_name_to_id_known() {
        assert_eq!(field_name_to_id("Amount"), 1_001);
        assert_eq!(field_name_to_id("Account"), 3_001);
        assert_eq!(field_name_to_id("Destination"), 3_003);
        assert_eq!(field_name_to_id("Fee"), 1_008);
        assert_eq!(field_name_to_id("Sequence"), 4);
    }

    #[test]
    fn field_name_to_id_unknown() {
        assert_eq!(field_name_to_id("SomeUnknownField"), 0);
    }

    #[test]
    fn populate_otxn_fields_from_tx() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Amount": "1000000",
            "Fee": "12",
        });
        let mut ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        populate_otxn_fields(&mut ctx, &tx);

        assert!(ctx.otxn_fields.contains_key(&2)); // TransactionType
        assert!(ctx.otxn_fields.contains_key(&3_001)); // Account
        assert!(ctx.otxn_fields.contains_key(&1_001)); // Amount
        assert!(ctx.otxn_fields.contains_key(&1_008)); // Fee
    }

    #[test]
    fn execute_hooks_no_destination() {
        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
        });
        let hash = Hash256::default();

        let result = execute_hooks_for_tx(&tx, &hash, &NoopView);
        assert!(result.is_none());
    }

    #[test]
    fn execute_hooks_no_hook_definition() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
        });
        let hash = Hash256::default();

        // NoopView returns None for all reads -> no hook definition found
        let result = execute_hooks_for_tx(&tx, &hash, &NoopView);
        assert!(result.is_none());
    }

    /// A minimal ReadView that returns None for all reads.
    struct NoopView;

    impl ReadView for NoopView {
        fn read(&self, _key: &Hash256) -> Option<Vec<u8>> {
            None
        }
        fn seq(&self) -> u32 {
            0
        }
        fn fees(&self) -> &FeeSettings {
            &NOOP_FEES
        }
        fn drops(&self) -> u64 {
            0
        }
        fn parent_close_time(&self) -> u32 {
            0
        }
    }

    // FeeSettings fields are all plain u64, so a const static is fine.
    static NOOP_FEES: FeeSettings = FeeSettings {
        base_fee: 10,
        reserve_base: 10_000_000,
        reserve_increment: 2_000_000,
    };
}
