use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultSetTransactor;

/// Parse the 32-byte `VaultID` (the vault keylet itself).
fn vault_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "VaultID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

impl Transactor for VaultSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        vault_id(ctx.tx)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let vault_key = vault_id(ctx.tx)?;
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Only the vault owner may modify it.
        let owner = vault["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let vault_key = vault_id(ctx.tx)?;
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            vault["Data"] = serde_json::Value::String(data.to_string());
        }

        if let Some(max) = helpers::get_u64_str_field(ctx.tx, "AssetsMaximum") {
            // A non-zero cap below the assets already held is rejected.
            if max != 0 {
                let total: u128 = vault
                    .get("AssetsTotal")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if (max as u128) < total {
                    return Err(TransactionResult::TecLimitExceeded);
                }
            }
            vault["AssetsMaximum"] = serde_json::Value::String(max.to_string());
        }

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Bump the owner's sequence.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
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

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const OTHER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_vault() -> (Ledger, rxrpl_primitives::Hash256) {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&owner_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 2,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let vault_key = keylet::vault(&owner_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Account": OTHER,
            "Owner": OWNER,
            "Sequence": 1,
            "ShareMPTID": "00000001A62B0DE19DFAF4D7C4E59DF8927BFF79FE146246",
            "WithdrawalPolicy": 1,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, vault_key)
    }

    fn vault_id_hex(vault_key: &rxrpl_primitives::Hash256) -> String {
        hex::encode_upper(vault_key.as_bytes())
    }

    #[test]
    fn update_assets_maximum() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultID": vault_id_hex(&vault_key),
            "AssetsMaximum": "75000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["AssetsMaximum"].as_str().unwrap(), "75000000");
    }

    #[test]
    fn update_data() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultID": vault_id_hex(&vault_key),
            "Data": "DEADBEEF",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["Data"].as_str().unwrap(), "DEADBEEF");
    }

    #[test]
    fn reject_missing_vault_id() {
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            VaultSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_nonexistent_vault() {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&owner_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultID": "00000000000000000000000000000000000000000000000000000000000000FF",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_non_owner() {
        let (mut ledger, vault_key) = setup_with_vault();
        let other_id = decode_account_id(OTHER).unwrap();
        let other_key = keylet::account(&other_id);
        let other_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OTHER,
            "Balance": "50000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(other_key, serde_json::to_vec(&other_acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OTHER,
            "VaultID": vault_id_hex(&vault_key),
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn increments_account_sequence() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultID": vault_id_hex(&vault_key),
            "AssetsMaximum": "100000000",
            "Fee": "12",
            "Sequence": 2,
        });

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        VaultSetTransactor.apply(&mut ctx).unwrap();

        let owner_id = decode_account_id(OWNER).unwrap();
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 3);
    }
}
