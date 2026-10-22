use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::handlers::credentials::{self, decode_non_zero_account, validate_credential_type};
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CredentialDeleteTransactor;

impl Transactor for CredentialDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amendment + flag-mask gate, before the transactor's field checks.
        credentials::preflight_gate(ctx.rules, ctx.tx)?;

        let subject = helpers::get_str_field(ctx.tx, "Subject");
        let issuer = helpers::get_str_field(ctx.tx, "Issuer");

        // At least one of Subject / Issuer must be present.
        if subject.is_none() && issuer.is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        // A present Subject / Issuer must not be the zeroed AccountID.
        if let Some(s) = subject {
            decode_non_zero_account(s)?;
        }
        if let Some(i) = issuer {
            decode_non_zero_account(i)?;
        }

        // sfCredentialType: present, non-empty, ≤ 64 bytes.
        validate_credential_type(ctx.tx)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // rippled checks existence only here; the permission test (and the
        // expired-credential exception) lives in doApply so that a nonexistent
        // credential returns tecNO_ENTRY even for a third party.
        let account_str = helpers::get_account(ctx.tx)?;

        // Subject and Issuer default to the submitting Account when absent.
        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap_or(account_str);
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap_or(account_str);
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType")
            .ok_or(TransactionResult::TemMalformed)?;
        let cred_key = keylet::credential(
            &subject_id,
            &issuer_id,
            &hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?,
        );

        if !ctx.view.exists(&cred_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap_or(account_str);
        let issuer_str = helpers::get_str_field(ctx.tx, "Issuer").unwrap_or(account_str);
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();

        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let issuer_id =
            decode_account_id(issuer_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let cred_key = keylet::credential(
            &subject_id,
            &issuer_id,
            &hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?,
        );

        let cred_bytes = ctx
            .view
            .read(&cred_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let cred: serde_json::Value =
            serde_json::from_slice(&cred_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Only the subject or the issuer may delete a live credential. A third
        // party is allowed to delete it only once it has expired (rippled
        // doApply: the permission check is gated on `!checkExpired`).
        if subject_id != account_id
            && issuer_id != account_id
            && !credentials::check_expired(&cred, ctx.view.parent_close_time())
        {
            return Err(TransactionResult::TecNoPermission);
        }

        // deleteSLE: unlink both directories (page-hinted), free the reserve from
        // the holder, erase the SLE.
        credentials::delete_credential(ctx.view, &cred_key, &cred)?;

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
    const CHARLIE: &str = "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy";

    fn rules() -> Rules {
        Rules::from_enabled([feature_id("Credentials"), feature_id("fixInvalidTxFlags")])
    }

    fn put_account(ledger: &mut Ledger, addr: &str, balance: u64, owner_count: u32) {
        let id = decode_account_id(addr).unwrap();
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": owner_count,
            "Flags": 0,
        });
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn link_dir(ledger: &mut Ledger, addr: &str, cred_key: &rxrpl_primitives::Hash256) {
        let id = decode_account_id(addr).unwrap();
        let dir_root = keylet::owner_dir(&id);
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

    /// Accepted credential: subject=BOB holds the reserve (OwnerCount=1), issuer=
    /// ALICE OwnerCount=0. Both owner dirs link the credential (single root page).
    fn setup_with_credential() -> Ledger {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ALICE, 100_000_000, 0);
        put_account(&mut ledger, BOB, 50_000_000, 1);

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        let entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Flags": credentials::LSF_ACCEPTED,
            "IssuerNode": "0000000000000000",
            "SubjectNode": "0000000000000000",
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        link_dir(&mut ledger, ALICE, &cred_key);
        link_dir(&mut ledger, BOB, &cred_key);
        ledger
    }

    #[test]
    fn preflight_no_subject_no_issuer() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
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
            CredentialDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_credential_type() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
            "Subject": BOB,
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
            CredentialDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_third_party_existing_credential_ok() {
        // The permission check moved to apply, so preclaim no longer rejects a
        // third party for an *existing* credential — it returns Ok.
        let mut ledger = setup_with_credential();
        put_account(&mut ledger, CHARLIE, 10_000_000, 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": CHARLIE,
            "Subject": BOB,
            "Issuer": ALICE,
            "CredentialType": "4B5943",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(CredentialDeleteTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn preclaim_credential_not_found() {
        // Existence (tecNO_ENTRY) is checked before permission, so even a third
        // party hitting a nonexistent credential gets tecNO_ENTRY.
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, CHARLIE, 100_000_000, 0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": CHARLIE,
            "Subject": BOB,
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
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": ALICE,
            "Subject": BOB,
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
            CredentialDeleteTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        assert!(!sandbox.exists(&cred_key));

        // Accepted credential: the subject (BOB) held the reserve, so its owner
        // count drops 1 → 0; the issuer is unchanged.
        let subject: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bob_id)).unwrap()).unwrap();
        assert_eq!(subject["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_subject_deletes_credential() {
        let ledger = setup_with_credential();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": BOB,
            "Subject": BOB,
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
            CredentialDeleteTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let alice_id = decode_account_id(ALICE).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        let cred_key = keylet::credential(&bob_id, &alice_id, b"KYC");
        assert!(!sandbox.exists(&cred_key));

        let subject: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&bob_id)).unwrap()).unwrap();
        assert_eq!(subject["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_third_party_non_expired_no_permission() {
        // A third party may not delete a live (non-expired) credential.
        let mut ledger = setup_with_credential();
        put_account(&mut ledger, CHARLIE, 10_000_000, 0);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": CHARLIE,
            "Subject": BOB,
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
            CredentialDeleteTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn apply_third_party_deletes_expired() {
        // An EXPIRED credential is deletable by anyone (rippled gates the
        // permission check on `!checkExpired`).
        let mut ledger = Ledger::genesis();
        ledger.header.parent_close_time = 1000;
        put_account(&mut ledger, ALICE, 100_000_000, 1); // issuer holds reserve
        put_account(&mut ledger, BOB, 50_000_000, 0);
        put_account(&mut ledger, CHARLIE, 10_000_000, 0);

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
        link_dir(&mut ledger, ALICE, &cred_key);
        link_dir(&mut ledger, BOB, &cred_key);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();
        let tx = serde_json::json!({
            "TransactionType": "CredentialDelete",
            "Account": CHARLIE,
            "Subject": BOB,
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
            CredentialDeleteTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        assert!(!sandbox.exists(&cred_key));
        // Unaccepted credential: the issuer held the reserve → ALICE 1 → 0.
        let issuer: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&alice_id)).unwrap()).unwrap();
        assert_eq!(issuer["OwnerCount"].as_u64().unwrap(), 0);
    }
}
