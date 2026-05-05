use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// lsfAccepted flag — set when issuer == subject (auto-accepted).
const LSF_ACCEPTED: u32 = 0x00010000;

/// Maximum length of CredentialType field in characters.
const MAX_CREDENTIAL_TYPE_LEN: usize = 128;

pub struct CredentialCreateTransactor;

impl Transactor for CredentialCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "Subject").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType")
            .ok_or(TransactionResult::TemMalformed)?;
        if credential_type.is_empty() {
            return Err(TransactionResult::TemMalformed);
        }
        if credential_type.len() > MAX_CREDENTIAL_TYPE_LEN {
            return Err(TransactionResult::TemMalformed);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let issuer_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, issuer_str)?;

        let subject_str =
            helpers::get_str_field(ctx.tx, "Subject").ok_or(TransactionResult::TemMalformed)?;
        // Subject account must exist; map to tecNO_TARGET (not terNO_ACCOUNT)
        // to match rippled's CredentialCreate semantics.
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let subject_key = keylet::account(&subject_id);
        if !ctx.view.exists(&subject_key) {
            return Err(TransactionResult::TecNoTarget);
        }

        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());
        if ctx.view.exists(&cred_key) {
            return Err(TransactionResult::TecDuplicate);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let issuer_str = helpers::get_account(ctx.tx)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let account_key = keylet::account(&issuer_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap();
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());

        // Auto-accept when issuer == subject (rippled behavior).
        let auto_accept = subject_id == issuer_id;
        let mut entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": subject_str,
            "Issuer": issuer_str,
            "CredentialType": credential_type,
            "Accepted": auto_accept,
            "Flags": if auto_accept { LSF_ACCEPTED } else { 0 },
        });
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            entry["URI"] = serde_json::Value::String(uri.to_string());
        }
        if let Some(expiration) = helpers::get_u32_field(ctx.tx, "Expiration") {
            entry["Expiration"] = serde_json::Value::from(expiration);
        }
        let entry_data = serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(cred_key, entry_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Add credential to issuer's owner directory so account_objects can
        // surface it via the ?type=credential filter.
        add_to_owner_dir(ctx.view, &issuer_id, &cred_key)?;

        helpers::adjust_owner_count(&mut account, 1);

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

    fn setup_two_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ALICE, 100_000_000u64), (BOB, 50_000_000)] {
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
    fn preflight_missing_subject() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "CredentialType": "KYC",
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
            CredentialCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_credential_type() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "",
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
            CredentialCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_valid() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "KYC",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(CredentialCreateTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preclaim_duplicate_credential() {
        let mut ledger = setup_two_accounts();
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Accepted": false,
            "Flags": 0,
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "KYC",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            CredentialCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }

    #[test]
    fn apply_creates_credential() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
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

        let result = CredentialCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        assert!(sandbox.exists(&cred_key));

        let entry_bytes = sandbox.read(&cred_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["Issuer"].as_str().unwrap(), ALICE);
        assert_eq!(entry["Subject"].as_str().unwrap(), BOB);
        assert_eq!(entry["Accepted"].as_bool().unwrap(), false);

        let account_key = keylet::account(&alice_id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
    }
}
