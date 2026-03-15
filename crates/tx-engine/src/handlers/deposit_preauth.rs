use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct DepositPreauthTransactor;

impl Transactor for DepositPreauthTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let has_authorize = helpers::get_str_field(ctx.tx, "Authorize").is_some();
        let has_unauthorize = helpers::get_str_field(ctx.tx, "Unauthorize").is_some();

        // Exactly one of Authorize or Unauthorize
        if has_authorize == has_unauthorize {
            return Err(TransactionResult::TemMalformed);
        }

        let account = helpers::get_account(ctx.tx)?;
        let target = if has_authorize {
            helpers::get_str_field(ctx.tx, "Authorize").unwrap()
        } else {
            helpers::get_str_field(ctx.tx, "Unauthorize").unwrap()
        };

        if account == target {
            return Err(TransactionResult::TemCannotPreAuthSelf);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        if let Some(authorize) = helpers::get_str_field(ctx.tx, "Authorize") {
            // Target account must exist
            helpers::read_account_by_address(ctx.view, authorize)?;

            // Check for duplicate preauth
            let owner_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let auth_id = decode_account_id(authorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&owner_id, &auth_id);
            if ctx.view.exists(&dp_key) {
                return Err(TransactionResult::TecDuplicate);
            }
        }

        if let Some(unauthorize) = helpers::get_str_field(ctx.tx, "Unauthorize") {
            let owner_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let auth_id = decode_account_id(unauthorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&owner_id, &auth_id);
            if !ctx.view.exists(&dp_key) {
                return Err(TransactionResult::TecNoEntry);
            }
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Update sender account (sequence + owner count)
        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        if let Some(authorize) = helpers::get_str_field(ctx.tx, "Authorize") {
            let auth_id = decode_account_id(authorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&account_id, &auth_id);

            let entry = serde_json::json!({
                "LedgerEntryType": "DepositPreauth",
                "Account": account_str,
                "Authorize": authorize,
                "Flags": 0,
            });
            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(dp_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut account, 1);
        }

        if let Some(unauthorize) = helpers::get_str_field(ctx.tx, "Unauthorize") {
            let auth_id = decode_account_id(unauthorize)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dp_key = keylet::deposit_preauth(&account_id, &auth_id);

            ctx.view
                .erase(&dp_key)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut account, -1);
        }

        let account_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(account_key, account_data)
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
    const TARGET: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_two_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (TARGET, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        ledger
    }

    #[test]
    fn preflight_both_authorize_and_unauthorize() {
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": TARGET,
            "Unauthorize": TARGET,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            DepositPreauthTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_self_preauth() {
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": OWNER,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            DepositPreauthTransactor.preflight(&ctx),
            Err(TransactionResult::TemCannotPreAuthSelf)
        );
    }

    #[test]
    fn apply_authorize_then_unauthorize() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // Authorize
        let tx = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Authorize": TARGET,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = DepositPreauthTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify entry exists
        let owner_id = decode_account_id(OWNER).unwrap();
        let target_id = decode_account_id(TARGET).unwrap();
        let dp_key = keylet::deposit_preauth(&owner_id, &target_id);
        assert!(sandbox.exists(&dp_key));

        // Verify owner count
        let owner_key = keylet::account(&owner_id);
        let owner_bytes = sandbox.read(&owner_key).unwrap();
        let owner: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
        assert_eq!(owner["OwnerCount"].as_u64().unwrap(), 1);

        // Unauthorize
        let tx2 = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": OWNER,
            "Unauthorize": TARGET,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx2 = ApplyContext {
            tx: &tx2,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result2 = DepositPreauthTransactor.apply(&mut ctx2).unwrap();
        assert_eq!(result2, TransactionResult::TesSuccess);

        // Entry deleted
        assert!(!sandbox.exists(&dp_key));

        // Owner count back to 0
        let owner_bytes = sandbox.read(&owner_key).unwrap();
        let owner: serde_json::Value = serde_json::from_slice(&owner_bytes).unwrap();
        assert_eq!(owner["OwnerCount"].as_u64().unwrap(), 0);
    }
}
