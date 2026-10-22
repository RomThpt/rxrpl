use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultDeleteTransactor;

/// Parse the 32-byte `VaultID` (the vault keylet itself).
fn vault_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "VaultID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

fn num(v: &serde_json::Value, field: &str) -> u128 {
    v.get(field)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

impl Transactor for VaultDeleteTransactor {
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

        if vault["Owner"].as_str() != Some(account_str) {
            return Err(TransactionResult::TecNoPermission);
        }
        if num(&vault, "AssetsAvailable") != 0 || num(&vault, "AssetsTotal") != 0 {
            return Err(TransactionResult::TecHasObligations);
        }

        let pseudo_id = decode_account_id(
            vault["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TecObjectNotFound)?;
        let issuance: serde_json::Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if num(&issuance, "OutstandingAmount") != 0 {
            return Err(TransactionResult::TecHasObligations);
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
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let pseudo_id = decode_account_id(
            vault["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);

        let mut owner_count_delta: i32 = 0;

        // 1. Remove the owner's (now-empty) share MPToken.
        let owner_mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &account_id);
        if ctx.view.exists(&owner_mptoken_key) {
            ctx.view
                .erase(&owner_mptoken_key)
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &owner_mptoken_key)?;
            owner_count_delta -= 1;
        }

        // 2. Remove the share issuance from the pseudo-account's directory and
        //    drop the pseudo's owner count for it.
        crate::owner_dir::remove_from_owner_dir(ctx.view, &pseudo_id, &issuance_key)?;
        ctx.view
            .erase(&issuance_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // 3. Erase the pseudo-account (owner count must reach 0 first).
        let pseudo_key = keylet::account(&pseudo_id);
        let pseudo_bytes = ctx
            .view
            .read(&pseudo_key)
            .ok_or(TransactionResult::TefBadLedger)?;
        let mut pseudo: serde_json::Value =
            serde_json::from_slice(&pseudo_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut pseudo, -1);
        ctx.view
            .update(
                pseudo_key,
                serde_json::to_vec(&pseudo).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .erase(&pseudo_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // 4. Remove the vault from the owner's directory and erase it.
        crate::owner_dir::remove_from_owner_dir(ctx.view, &account_id, &vault_key)?;
        ctx.view
            .erase(&vault_key)
            .map_err(|_| TransactionResult::TefInternal)?;
        owner_count_delta -= 2;

        // 5. Settle the owner's account: owner-count delta and sequence bump.
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut account, owner_count_delta);
        ctx.view
            .update(
                acct_key,
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const PSEUDO: &str = "rG9ckJcta51jT4iYdBiGo7du8MsKh7fzXp";
    const SHARE_ID: &str = "00000001A62B0DE19DFAF4D7C4E59DF8927BFF79FE146246";

    fn setup() -> (Ledger, Hash256) {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 3);
        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        let issuance_key = keylet::mptoken_issuance(&pseudo_id, 1);
        let owner_mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &owner_id);

        ledger
            .put_state(
                keylet::account(&owner_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": OWNER,
                    "Balance": "90000000", "Sequence": 8, "OwnerCount": 3, "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                keylet::account(&pseudo_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "AccountRoot", "Account": PSEUDO,
                    "Balance": "0", "Flags": 26214400, "OwnerCount": 1,
                    "VaultID": hex::encode_upper(vault_key.as_bytes()),
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                issuance_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "MPTokenIssuance", "Flags": 56, "Issuer": PSEUDO,
                    "Sequence": 1, "OutstandingAmount": "0",
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                owner_mptoken_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "MPToken", "Account": OWNER,
                    "MPTokenIssuanceID": SHARE_ID, "MPTAmount": "0",
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                vault_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Vault", "Account": PSEUDO, "Owner": OWNER,
                    "Sequence": 3, "ShareMPTID": SHARE_ID, "WithdrawalPolicy": 1,
                }))
                .unwrap(),
            )
            .unwrap();
        // Owner directory holding the vault and the share MPToken.
        let owner_dir = keylet::owner_dir(&owner_id);
        ledger
            .put_state(
                owner_dir,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode", "Flags": 0,
                    "RootIndex": hex::encode_upper(owner_dir.as_bytes()),
                    "Owner": OWNER,
                    "Indexes": [
                        hex::encode_upper(vault_key.as_bytes()),
                        hex::encode_upper(owner_mptoken_key.as_bytes()),
                    ],
                }))
                .unwrap(),
            )
            .unwrap();
        let pseudo_dir = keylet::owner_dir(&pseudo_id);
        ledger
            .put_state(
                pseudo_dir,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode", "Flags": 0,
                    "RootIndex": hex::encode_upper(pseudo_dir.as_bytes()),
                    "Owner": PSEUDO,
                    "Indexes": [hex::encode_upper(issuance_key.as_bytes())],
                }))
                .unwrap(),
            )
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn deletes_empty_vault() {
        let (ledger, vault_key) = setup();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
            "Account": OWNER,
            "VaultID": hex::encode_upper(vault_key.as_bytes()),
            "Fee": "20",
            "Sequence": 8,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            VaultDeleteTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        assert!(sandbox.read(&vault_key).is_none());
        let pseudo_id = decode_account_id(PSEUDO).unwrap();
        assert!(sandbox.read(&keylet::account(&pseudo_id)).is_none());
        assert!(
            sandbox
                .read(&keylet::mptoken_issuance(&pseudo_id, 1))
                .is_none()
        );

        let owner_id = decode_account_id(OWNER).unwrap();
        let acct: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&owner_id)).unwrap()).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }
}
