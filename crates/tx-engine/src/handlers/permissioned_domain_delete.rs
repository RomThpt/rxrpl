use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PermissionedDomainDeleteTransactor;

impl Transactor for PermissionedDomainDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_u32_field(ctx.tx, "DomainID").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let seq = helpers::get_u32_field(ctx.tx, "DomainID").unwrap();
        let domain_key = keylet::permissioned_domain(&account_id, seq);
        if !ctx.view.exists(&domain_key) {
            return Err(TransactionResult::TecNoEntry);
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

        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let seq = helpers::get_u32_field(ctx.tx, "DomainID").unwrap();
        let domain_key = keylet::permissioned_domain(&account_id, seq);
        ctx.view
            .erase(&domain_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        helpers::adjust_owner_count(&mut account, -1);

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

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_account_with_domain() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();

        let account_key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 5,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(account_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let domain_key = keylet::permissioned_domain(&id, 3);
        let domain = serde_json::json!({
            "LedgerEntryType": "PermissionedDomain",
            "Owner": ALICE,
            "Sequence": 3,
            "AcceptedCredentials": [{"Issuer": "rXYZ"}],
            "Flags": 0,
        });
        ledger
            .put_state(domain_key, serde_json::to_vec(&domain).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_domain_id_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            PermissionedDomainDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_with_domain_id_ok() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": 3,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(PermissionedDomainDeleteTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preclaim_no_domain_rejects() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
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
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": 99,
            "Fee": "12",
        });
        let ctx = PreclaimContext { tx: &tx, view: &view, rules: &rules };
        assert_eq!(
            PermissionedDomainDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_deletes_domain() {
        let ledger = setup_account_with_domain();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": 3,
            "Fee": "12",
            "Sequence": 5,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 3);
        assert!(!sandbox.exists(&domain_key));

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 6);
    }

    #[test]
    fn preclaim_existing_domain_ok() {
        let ledger = setup_account_with_domain();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": 3,
            "Fee": "12",
        });
        let ctx = PreclaimContext { tx: &tx, view: &view, rules: &rules };
        assert_eq!(PermissionedDomainDeleteTransactor.preclaim(&ctx), Ok(()));
    }
}
