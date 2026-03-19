use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CredentialDeleteTransactor;

impl Transactor for CredentialDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "Subject").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        if helpers::get_str_field(ctx.tx, "Issuer").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        if helpers::get_str_field(ctx.tx, "CredentialType").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap();
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap();

        // Account must be either Subject or Issuer
        if account_str != subject_str && account_str != issuer_str {
            return Err(TransactionResult::TecNoPermission);
        }

        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());

        if !ctx.view.exists(&cred_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap();
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap();
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();

        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());

        ctx.view
            .erase(&cred_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Adjust owner count on the issuer
        if account_str == issuer_str {
            helpers::adjust_owner_count(&mut account, -1);
        } else {
            // Account is subject; decrement issuer's owner count separately
            let issuer_account_key = keylet::account(&issuer_id);
            let issuer_bytes = ctx
                .view
                .read(&issuer_account_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut issuer_account: serde_json::Value = serde_json::from_slice(&issuer_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

            helpers::adjust_owner_count(&mut issuer_account, -1);

            let issuer_data =
                serde_json::to_vec(&issuer_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(issuer_account_key, issuer_data)
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
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const CHARLIE: &str = "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy";

    fn setup_with_credential() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance, owner_count) in [(ALICE, 100_000_000u64, 1u32), (BOB, 50_000_000, 0)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": owner_count,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Accepted": true,
            "Flags": 0,
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_fields() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
            "Subject": BOB,
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
            CredentialDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_not_subject_or_issuer() {
        let mut ledger = setup_with_credential();
        // Add Charlie
        let charlie_id = decode_account_id(CHARLIE).unwrap();
        let key = keylet::account(&charlie_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": CHARLIE,
            "Balance": "10000000",
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
            "TransactionType": "CredentialDelete",
            "Account": CHARLIE,
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            CredentialDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn preclaim_credential_not_found() {
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
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            CredentialDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_issuer_deletes_credential() {
        let ledger = setup_with_credential();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CredentialDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        assert!(!sandbox.exists(&cred_key));

        let account_key = keylet::account(&alice_id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_subject_deletes_credential() {
        let ledger = setup_with_credential();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": BOB,
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CredentialDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        assert!(!sandbox.exists(&cred_key));

        // Issuer's owner count should decrease
        let issuer_key = keylet::account(&alice_id);
        let issuer_bytes = sandbox.read(&issuer_key).unwrap();
        let issuer: serde_json::Value = serde_json::from_slice(&issuer_bytes).unwrap();
        assert_eq!(issuer["OwnerCount"].as_u64().unwrap(), 0);
    }
}
