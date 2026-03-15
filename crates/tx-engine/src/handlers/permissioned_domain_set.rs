use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PermissionedDomainSetTransactor;

impl Transactor for PermissionedDomainSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let credentials = helpers::get_array_field(ctx.tx, "AcceptedCredentials")
            .ok_or(TransactionResult::TemMalformed)?;
        if credentials.is_empty() {
            return Err(TransactionResult::TecArrayEmpty);
        }
        if credentials.len() > 10 {
            return Err(TransactionResult::TecArrayTooLarge);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
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

        // Try to find an existing domain using the DomainID field (sequence number)
        let domain_seq = helpers::get_u32_field(ctx.tx, "DomainID");

        let (domain_key, is_create) = if let Some(seq) = domain_seq {
            let key = keylet::permissioned_domain(&account_id, seq);
            if ctx.view.exists(&key) {
                (key, false)
            } else {
                // DomainID provided but not found: create with given seq
                (key, true)
            }
        } else {
            // No DomainID: create new domain using account sequence
            let seq = helpers::get_sequence(&account) - 1; // sequence was already incremented
            let key = keylet::permissioned_domain(&account_id, seq);
            (key, true)
        };

        let credentials = ctx.tx.get("AcceptedCredentials").cloned()
            .ok_or(TransactionResult::TemMalformed)?;

        if is_create {
            let seq = if let Some(s) = domain_seq {
                s
            } else {
                helpers::get_sequence(&account) - 1
            };

            let entry = serde_json::json!({
                "LedgerEntryType": "PermissionedDomain",
                "Owner": account_str,
                "Sequence": seq,
                "AcceptedCredentials": credentials,
                "Flags": 0,
            });
            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(domain_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, 1);
        } else {
            let entry_bytes = ctx
                .view
                .read(&domain_key)
                .ok_or(TransactionResult::TefInternal)?;
            let mut entry: serde_json::Value =
                serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;
            entry["AcceptedCredentials"] = credentials;
            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(domain_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
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

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_account() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 5,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_credentials_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_credentials_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [],
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TecArrayEmpty)
        );
    }

    #[test]
    fn preflight_too_many_credentials_rejects() {
        let creds: Vec<serde_json::Value> = (0..11)
            .map(|i| serde_json::json!({"Issuer": format!("issuer{i}")}))
            .collect();
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": creds,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            PermissionedDomainSetTransactor.preflight(&ctx),
            Err(TransactionResult::TecArrayTooLarge)
        );
    }

    #[test]
    fn apply_creates_domain() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "AcceptedCredentials": [{"Issuer": "rXYZ", "CredentialType": "KYC"}],
            "Fee": "12",
            "Sequence": 5,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Domain created with sequence 5 (account seq before increment was 5,
        // after increment is 6, domain seq = 6 - 1 = 5)
        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 5);
        assert!(sandbox.exists(&domain_key));

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 6);
    }

    #[test]
    fn apply_updates_existing_domain() {
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let domain_key = keylet::permissioned_domain(&id, 3);
        let existing = serde_json::json!({
            "LedgerEntryType": "PermissionedDomain",
            "Owner": ALICE,
            "Sequence": 3,
            "AcceptedCredentials": [{"Issuer": "old"}],
            "Flags": 0,
        });
        ledger
            .put_state(domain_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainSet",
            "Account": ALICE,
            "DomainID": 3,
            "AcceptedCredentials": [{"Issuer": "new"}],
            "Fee": "12",
            "Sequence": 5,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let entry_bytes = sandbox.read(&domain_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["AcceptedCredentials"][0]["Issuer"].as_str().unwrap(), "new");

        // Owner count should not increase on update
        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
    }
}
