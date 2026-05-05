use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CredentialAcceptTransactor;

impl Transactor for CredentialAcceptTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "Issuer").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        if helpers::get_str_field(ctx.tx, "CredentialType").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let subject_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, subject_str)?;

        let issuer_str =
            helpers::get_str_field(ctx.tx, "Issuer").ok_or(TransactionResult::TemMalformed)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType")
            .ok_or(TransactionResult::TemMalformed)?;

        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());

        let entry_bytes = ctx
            .view
            .read(&cred_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let entry: serde_json::Value =
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // The `Accepted` boolean is dropped on round-trip through the XRPL
        // binary SLE codec (Credential entries persist acceptance via the
        // lsfAccepted flag, 0x00010000). Check both so the duplicate guard
        // fires whether the entry was just written by this handler or read
        // back from the ledger after binary encoding.
        const LSF_ACCEPTED: u32 = 0x00010000;
        let accepted_field = entry
            .get("Accepted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let accepted_flag = entry
            .get("Flags")
            .and_then(|v| v.as_u64())
            .map(|f| (f as u32) & LSF_ACCEPTED != 0)
            .unwrap_or(false);
        if accepted_field || accepted_flag {
            return Err(TransactionResult::TecDuplicate);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let subject_str = helpers::get_account(ctx.tx)?;
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let account_key = keylet::account(&subject_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut account);

        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap();
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let cred_key = keylet::credential(&subject_id, &issuer_id, credential_type.as_bytes());

        let entry_bytes = ctx
            .view
            .read(&cred_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut entry: serde_json::Value =
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

        entry["Accepted"] = serde_json::Value::Bool(true);
        // Also set lsfAccepted flag (0x00010000) so account_objects responses
        // expose the accepted status via the Flags field, matching rippled.
        const LSF_ACCEPTED: u32 = 0x00010000;
        let prev_flags = entry.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        entry["Flags"] = serde_json::Value::from(prev_flags | LSF_ACCEPTED);

        let entry_data = serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(cred_key, entry_data)
            .map_err(|_| TransactionResult::TefInternal)?;

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

    fn setup_with_credential(accepted: bool) -> Ledger {
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

        // Credential: issuer=ALICE, subject=BOB
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Accepted": accepted,
            "Flags": 0,
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_issuer() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
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
            CredentialAcceptTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_credential_type() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
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
            CredentialAcceptTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_credential_not_found() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(BOB).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BOB,
            "Balance": "50000000",
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
            "TransactionType": "CredentialAccept",
            "Account": BOB,
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
            CredentialAcceptTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn preclaim_already_accepted() {
        let ledger = setup_with_credential(true);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
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
            CredentialAcceptTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }

    #[test]
    fn preclaim_already_accepted_via_flag_only() {
        // After binary SLE round-trip, the `Accepted` field is dropped and
        // only the lsfAccepted flag (0x00010000) survives. The duplicate
        // guard must fire on the flag alone.
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
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        // Note: no "Accepted" key — only Flags carries the accepted state.
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "KYC",
            "Flags": 0x00010000u32,
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
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
            CredentialAcceptTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }

    #[test]
    fn apply_accepts_credential() {
        let ledger = setup_with_credential(false);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
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

        let result = CredentialAcceptTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry_bytes = sandbox.read(&cred_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["Accepted"].as_bool().unwrap(), true);
    }
}
