use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::handlers::credentials::{
    self, LSF_ACCEPTED, decode_non_zero_account, validate_credential_type,
};
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CredentialAcceptTransactor;

impl Transactor for CredentialAcceptTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amendment + flag-mask gate, before the transactor's field checks.
        credentials::preflight_gate(ctx.rules, ctx.tx)?;

        // sfIssuer must be present and non-zero (rippled: temINVALID_ACCOUNT_ID
        // when the Issuer field is absent / zeroed).
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer")
            .ok_or(TransactionResult::TemInvalidAccountId)?;
        decode_non_zero_account(issuer_str)?;

        // sfCredentialType: present, non-empty, ≤ 64 bytes.
        validate_credential_type(ctx.tx)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let subject_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, subject_str)?;

        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer")
            .ok_or(TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType")
            .ok_or(TransactionResult::TemMalformed)?;

        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // The issuer account must still exist (rippled checks this first → a gone
        // issuer is tecNO_ISSUER, distinct from a missing credential's tecNO_ENTRY).
        if !ctx.view.exists(&keylet::account(&issuer_id)) {
            return Err(TransactionResult::TecNoIssuer);
        }

        let cred_key = keylet::credential(
            &subject_id,
            &issuer_id,
            &hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?,
        );

        let entry_bytes = ctx
            .view
            .read(&cred_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let entry: serde_json::Value =
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Acceptance is persisted only via lsfAccepted (the field survives the
        // binary SLE round-trip). An already-accepted credential is a duplicate.
        if helpers::get_flags(&entry) & LSF_ACCEPTED != 0 {
            return Err(TransactionResult::TecDuplicate);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let subject_str = helpers::get_account(ctx.tx)?;
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let subject_key = keylet::account(&subject_id);
        let subject_bytes = ctx
            .view
            .read(&subject_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut subject_acct: serde_json::Value =
            serde_json::from_slice(&subject_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap();
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let cred_key = keylet::credential(
            &subject_id,
            &issuer_id,
            &hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?,
        );

        // Owner-reserve gate (rippled doApply, checked BEFORE expiration): the
        // credential's reserve transfers to the subject on accept, so the subject
        // must afford one more owned object. CLAIMED tec.
        let reserve = ctx
            .fees
            .account_reserve(helpers::get_owner_count(&subject_acct) + 1);
        if helpers::get_balance(&subject_acct) < reserve {
            return Err(TransactionResult::TecInsufficientReserve);
        }

        let cred_bytes = ctx
            .view
            .read(&cred_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut cred: serde_json::Value =
            serde_json::from_slice(&cred_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // An expired credential is erased on accept (both directories unlinked,
        // reserve freed) and the transaction CLAIMS tecEXPIRED. Returning `Ok`
        // commits the deletion (an `Err` tec would discard it).
        if credentials::check_expired(&cred, ctx.view.parent_close_time()) {
            credentials::delete_credential(ctx.view, &cred_key, &cred)?;
            return Ok(TransactionResult::TecExpired);
        }

        // Accept: set lsfAccepted on the credential.
        let prev_flags = helpers::get_flags(&cred);
        cred["Flags"] = serde_json::Value::from(prev_flags | LSF_ACCEPTED);
        let cred_data = serde_json::to_vec(&cred).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(cred_key, cred_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Reserve ownership transfers from the issuer to the subject:
        // subject OwnerCount +1, issuer OwnerCount -1.
        helpers::adjust_owner_count(&mut subject_acct, 1);
        let subject_data =
            serde_json::to_vec(&subject_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(subject_key, subject_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        let issuer_key = keylet::account(&issuer_id);
        let issuer_bytes = ctx
            .view
            .read(&issuer_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut issuer_acct: serde_json::Value =
            serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut issuer_acct, -1);
        let issuer_data =
            serde_json::to_vec(&issuer_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(issuer_key, issuer_data)
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
    use rxrpl_amendment::feature::feature_id;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn rules() -> Rules {
        Rules::from_enabled([feature_id("Credentials"), feature_id("fixInvalidTxFlags")])
    }

    fn account(addr: &str, balance: u64, owner_count: u32) -> serde_json::Value {
        serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": owner_count,
            "Flags": 0,
        })
    }

    /// Issuer=ALICE (owns the credential, OwnerCount=1), subject=BOB. The
    /// credential carries lsfAccepted iff `accepted`, plus the node hints rippled
    /// records (single-page → "0"). Unaccepted credentials link both owner dirs.
    fn setup_with_credential(accepted: bool) -> Ledger {
        let mut ledger = Ledger::genesis();
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        ledger
            .put_state(
                keylet::account(&alice_id),
                serde_json::to_vec(&account(ALICE, 100_000_000, 1)).unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                keylet::account(&bob_id),
                serde_json::to_vec(&account(BOB, 50_000_000, 0)).unwrap(),
            )
            .unwrap();

        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Flags": if accepted { LSF_ACCEPTED } else { 0 },
            "IssuerNode": "0000000000000000",
            "SubjectNode": "0000000000000000",
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        // Link the credential into both owner directories (single root page).
        for (addr, owner) in [(ALICE, &alice_id), (BOB, &bob_id)] {
            let dir_root = keylet::owner_dir(owner);
            let dir = serde_json::json!({
                "LedgerEntryType": "DirectoryNode",
                "Owner": addr,
                "RootIndex": dir_root.to_string().to_uppercase(),
                "Indexes": [cred_key.to_string().to_uppercase()],
                "Flags": 0,
            });
            ledger
                .put_state(
                    keylet::dir_node(&dir_root, 0),
                    serde_json::to_vec(&dir).unwrap(),
                )
                .unwrap();
        }

        ledger
    }

    #[test]
    fn preflight_amendment_disabled() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
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
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn preflight_missing_issuer() {
        // Absent Issuer → temINVALID_ACCOUNT_ID (rippled), not temMALFORMED.
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "CredentialType": "4B5943",
            "Fee": "12",
        });
        let rules = rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            CredentialAcceptTransactor.preflight(&ctx),
            Err(TransactionResult::TemInvalidAccountId)
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
        let rules = rules();
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
    fn preclaim_no_issuer() {
        // Subject exists but the issuer account is gone → tecNO_ISSUER (checked
        // before the credential lookup).
        let mut ledger = Ledger::genesis();
        let bob_id = decode_account_id(BOB).unwrap();
        ledger
            .put_state(
                keylet::account(&bob_id),
                serde_json::to_vec(&account(BOB, 50_000_000, 0)).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            CredentialAcceptTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoIssuer)
        );
    }

    #[test]
    fn preclaim_credential_not_found() {
        // Both accounts exist but no credential → tecNO_ENTRY.
        let mut ledger = Ledger::genesis();
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        ledger
            .put_state(
                keylet::account(&alice_id),
                serde_json::to_vec(&account(ALICE, 100_000_000, 0)).unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                keylet::account(&bob_id),
                serde_json::to_vec(&account(BOB, 50_000_000, 0)).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
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
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
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
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
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
        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&cred_key).unwrap()).unwrap();
        assert!(entry["Flags"].as_u64().unwrap() as u32 & LSF_ACCEPTED != 0);

        // Reserve transfers: issuer 1 → 0, subject 0 → 1.
        let issuer: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&alice_id)).unwrap()).unwrap();
        let subject: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bob_id)).unwrap()).unwrap();
        assert_eq!(issuer["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(subject["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn apply_insufficient_reserve() {
        let mut ledger = setup_with_credential(false);
        // Drop the subject below accountReserve(1) (= 12 XRP at default fees).
        let bob_id = decode_account_id(BOB).unwrap();
        ledger
            .put_state(
                keylet::account(&bob_id),
                serde_json::to_vec(&account(BOB, 5_000_000, 0)).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            CredentialAcceptTransactor.apply(&mut ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
    }

    #[test]
    fn apply_expired_deletes_and_returns_tec_expired() {
        let mut ledger = setup_with_credential(false);
        ledger.header.parent_close_time = 1000;
        // Re-seed the credential with an Expiration strictly behind close time.
        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Flags": 0,
            "Expiration": 500u32,
            "IssuerNode": "0000000000000000",
            "SubjectNode": "0000000000000000",
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialAccept",
            "Account": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            CredentialAcceptTransactor.apply(&mut ctx),
            Ok(TransactionResult::TecExpired)
        );

        // Expired credential is erased; the unaccepted reserve was held by the
        // issuer, so only the issuer's owner count drops (subject unchanged).
        assert!(!sandbox.exists(&cred_key));
        let issuer: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&alice_id)).unwrap()).unwrap();
        let subject: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bob_id)).unwrap()).unwrap();
        assert_eq!(issuer["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(subject["OwnerCount"].as_u64().unwrap(), 0);
    }
}
