use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// Maximum number of hooks per account.
const MAX_HOOKS: usize = 10;

/// Maximum WASM binary size: 64 KiB (hex-encoded = 128 KiB).
const MAX_WASM_HEX_LEN: usize = 64 * 1024 * 2;

/// SetHook transaction handler.
///
/// Installs, updates, or removes WASM hooks on an account. Each account
/// may have up to 10 hooks. The Hooks array in the transaction specifies
/// the desired hook configuration.
pub struct SetHookTransactor;

impl Transactor for SetHookTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let hooks = helpers::get_array_field(ctx.tx, "Hooks")
            .ok_or(TransactionResult::TemMalformed)?;

        if hooks.is_empty() || hooks.len() > MAX_HOOKS {
            return Err(TransactionResult::TemMalformed);
        }

        for hook_wrapper in hooks {
            let hook = hook_wrapper
                .get("Hook")
                .ok_or(TransactionResult::TemMalformed)?;

            // If CreateCode is present, validate it
            if let Some(code_hex) = hook.get("CreateCode").and_then(|v| v.as_str()) {
                // Empty CreateCode means delete
                if !code_hex.is_empty() {
                    if code_hex.len() > MAX_WASM_HEX_LEN {
                        return Err(TransactionResult::TemMalformed);
                    }

                    // Validate hex encoding
                    let wasm_bytes = hex::decode(code_hex)
                        .map_err(|_| TransactionResult::TemMalformed)?;

                    // Validate WASM magic number (0x00 0x61 0x73 0x6D)
                    if wasm_bytes.len() < 4
                        || wasm_bytes[0] != 0x00
                        || wasm_bytes[1] != 0x61
                        || wasm_bytes[2] != 0x73
                        || wasm_bytes[3] != 0x6D
                    {
                        return Err(TransactionResult::TemMalformed);
                    }
                }
            }

            // HookHash, if present, must be 64 hex chars
            if let Some(hook_hash) = hook.get("HookHash").and_then(|v| v.as_str()) {
                if hook_hash.len() != 64 || hex::decode(hook_hash).is_err() {
                    return Err(TransactionResult::TemMalformed);
                }
            }

            // HookNamespace, if present, must be 64 hex chars
            if let Some(ns) = hook.get("HookNamespace").and_then(|v| v.as_str()) {
                if ns.len() != 64 || hex::decode(ns).is_err() {
                    return Err(TransactionResult::TemMalformed);
                }
            }

            // Must have at least CreateCode or HookHash
            let has_create_code = hook.get("CreateCode").is_some();
            let has_hook_hash = hook.get("HookHash").is_some();
            if !has_create_code && !has_hook_hash {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let _ = helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Read and update source account
        let src_key = keylet::account(&account_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let hooks = helpers::get_array_field(ctx.tx, "Hooks")
            .ok_or(TransactionResult::TemMalformed)?
            .clone();

        // Build the hook entries for the ledger object
        let hook_def_key = keylet::hook_definition(&account_id);
        let existing_def = ctx.view.read(&hook_def_key);

        let mut hook_entries = serde_json::json!([]);
        let entries = hook_entries.as_array_mut().unwrap();

        for hook_wrapper in &hooks {
            let hook = hook_wrapper
                .get("Hook")
                .ok_or(TransactionResult::TemMalformed)?;

            let create_code = hook
                .get("CreateCode")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if create_code.is_empty() && hook.get("HookHash").is_none() {
                // Delete slot: skip
                continue;
            }

            let mut entry = serde_json::json!({});

            if !create_code.is_empty() {
                entry["CreateCode"] = serde_json::Value::String(create_code.to_string());

                // Compute HookHash as SHA-256 of the WASM bytes
                let wasm_bytes = hex::decode(create_code)
                    .map_err(|_| TransactionResult::TefInternal)?;
                let hash = rxrpl_crypto::sha512_half::sha512_half(&[&wasm_bytes]);
                entry["HookHash"] = serde_json::Value::String(hex::encode(hash.as_bytes()));
            } else if let Some(hh) = hook.get("HookHash") {
                entry["HookHash"] = hh.clone();
            }

            if let Some(ns) = hook.get("HookNamespace") {
                entry["HookNamespace"] = ns.clone();
            }
            if let Some(on) = hook.get("HookOn") {
                entry["HookOn"] = on.clone();
            }
            if let Some(ver) = hook.get("HookApiVersion") {
                entry["HookApiVersion"] = ver.clone();
            }
            if let Some(params) = hook.get("HookParameters") {
                entry["HookParameters"] = params.clone();
            }
            if let Some(grants) = hook.get("HookGrants") {
                entry["HookGrants"] = grants.clone();
            }

            entries.push(entry);
        }

        if entries.is_empty() {
            // All hooks deleted: remove the HookDefinition entry
            if existing_def.is_some() {
                ctx.view
                    .erase(&hook_def_key)
                    .map_err(|_| TransactionResult::TefInternal)?;
                helpers::adjust_owner_count(&mut src_account, -1);
            }
        } else {
            let hook_def = serde_json::json!({
                "LedgerEntryType": "HookDefinition",
                "Account": account_str,
                "Hooks": hook_entries,
            });

            let hook_data =
                serde_json::to_vec(&hook_def).map_err(|_| TransactionResult::TefInternal)?;

            if existing_def.is_some() {
                ctx.view
                    .update(hook_def_key, hook_data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            } else {
                ctx.view
                    .insert(hook_def_key, hook_data)
                    .map_err(|_| TransactionResult::TefInternal)?;
                helpers::adjust_owner_count(&mut src_account, 1);
            }
        }

        helpers::increment_sequence(&mut src_account);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    /// Minimal valid WASM module (magic + version) hex-encoded.
    fn minimal_wasm_hex() -> String {
        // Minimal WASM: magic (0061736d) + version (01000000) +
        // type section with func type (param i32, result i64) +
        // function section + export section for "hook" + code section
        let wasm = wat::parse_str(
            r#"
            (module
                (func $hook (export "hook") (param i32) (result i64)
                    i64.const 0
                )
            )
            "#,
        )
        .expect("valid WAT");
        hex::encode(&wasm)
    }

    fn setup_ledger(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_rejects_empty_hooks() {
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "10",
            "Hooks": [],
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_rejects_missing_hooks() {
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "10",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_accepts_valid_hook() {
        let wasm_hex = minimal_wasm_hex();
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "10",
            "Hooks": [
                {
                    "Hook": {
                        "CreateCode": wasm_hex,
                        "HookNamespace": "A".repeat(64),
                        "HookOn": "0000000000000000",
                    }
                }
            ],
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(SetHookTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn preflight_rejects_invalid_wasm_magic() {
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "10",
            "Hooks": [
                {
                    "Hook": {
                        "CreateCode": "deadbeef",
                        "HookNamespace": "A".repeat(64),
                    }
                }
            ],
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_rejects_bad_hook_hash_length() {
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "10",
            "Hooks": [
                {
                    "Hook": {
                        "HookHash": "ABCD",
                    }
                }
            ],
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_creates_hook_definition() {
        let wasm_hex = minimal_wasm_hex();
        let ledger = setup_ledger(SRC, 100_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "12",
            "Sequence": 1,
            "Hooks": [
                {
                    "Hook": {
                        "CreateCode": wasm_hex,
                        "HookNamespace": "A".repeat(64),
                        "HookOn": "0000000000000000",
                    }
                }
            ],
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = SetHookTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify HookDefinition was created
        let account_id = decode_account_id(SRC).unwrap();
        let hook_key = keylet::hook_definition(&account_id);
        let hook_bytes = sandbox.read(&hook_key).unwrap();
        let hook_def: serde_json::Value = serde_json::from_slice(&hook_bytes).unwrap();
        assert_eq!(hook_def["LedgerEntryType"], "HookDefinition");
        assert_eq!(hook_def["Account"], SRC);
        assert_eq!(hook_def["Hooks"].as_array().unwrap().len(), 1);

        // Verify owner count increased
        let src_key = keylet::account(&account_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn apply_delete_hook_with_empty_create_code() {
        let wasm_hex = minimal_wasm_hex();
        let ledger = setup_ledger(SRC, 100_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // First, create a hook
        let tx1 = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "12",
            "Sequence": 1,
            "Hooks": [
                {
                    "Hook": {
                        "CreateCode": wasm_hex,
                        "HookNamespace": "A".repeat(64),
                    }
                }
            ],
        });

        let mut ctx1 = ApplyContext {
            tx: &tx1,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.apply(&mut ctx1).unwrap(),
            TransactionResult::TesSuccess
        );

        // Then delete it
        let tx2 = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": SRC,
            "Fee": "12",
            "Sequence": 2,
            "Hooks": [
                {
                    "Hook": {
                        "CreateCode": "",
                    }
                }
            ],
        });

        let mut ctx2 = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.apply(&mut ctx2).unwrap(),
            TransactionResult::TesSuccess
        );

        // Verify HookDefinition was removed
        let account_id = decode_account_id(SRC).unwrap();
        let hook_key = keylet::hook_definition(&account_id);
        assert!(sandbox.read(&hook_key).is_none());
    }
}
