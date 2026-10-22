use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::handlers::credentials::{
    self, LSF_ACCEPTED, MAX_CREDENTIAL_URI_LEN, validate_credential_type,
};
use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CredentialCreateTransactor;

impl Transactor for CredentialCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amendment + flag-mask gate (rippled framework: invoke_preflight +
        // preflight0), before the transactor's own field checks.
        credentials::preflight_gate(ctx.rules, ctx.tx)?;

        if helpers::get_str_field(ctx.tx, "Subject").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        // sfURI is optional but, when present, must be a non-empty blob ≤ 256
        // bytes (rippled `kMaxCredentialUriLength`).
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            let bytes = hex::decode(uri).map_err(|_| TransactionResult::TemMalformed)?;
            if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_URI_LEN {
                return Err(TransactionResult::TemMalformed);
            }
        }

        // sfCredentialType: present, non-empty, ≤ 64 bytes.
        validate_credential_type(ctx.tx)?;

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
        let cred_key = keylet::credential(
            &subject_id,
            &issuer_id,
            &hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?,
        );
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

        let subject_str = helpers::get_str_field(ctx.tx, "Subject").unwrap();
        let subject_id =
            decode_account_id(subject_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let credential_type = helpers::get_str_field(ctx.tx, "CredentialType").unwrap();
        // The keylet hashes the decoded CredentialType bytes, not the hex string.
        let ct_bytes = hex::decode(credential_type).map_err(|_| TransactionResult::TemMalformed)?;
        let cred_key = keylet::credential(&subject_id, &issuer_id, &ct_bytes);

        // Expiration is validated first (rippled doApply): a credential whose
        // Expiration is already strictly behind the parent close time is rejected
        // with a CLAIMED tecEXPIRED before anything is created.
        let expiration = helpers::get_u32_field(ctx.tx, "Expiration");
        if let Some(exp) = expiration {
            if ctx.view.parent_close_time() > exp {
                return Err(TransactionResult::TecExpired);
            }
        }

        // Owner-reserve gate (rippled doApply): the issuer must afford the reserve
        // for one more owned object. The engine consumed the fee centrally before
        // apply, so `get_balance` is rippled's post-fee balance. CLAIMED tec.
        let reserve = ctx
            .fees
            .account_reserve(helpers::get_owner_count(&account) + 1);
        if helpers::get_balance(&account) < reserve {
            return Err(TransactionResult::TecInsufficientReserve);
        }

        // A self-issued credential (subject == issuer) is auto-accepted
        // (lsfAccepted); a third-party credential starts unaccepted (Flags = 0).
        // sfFlags is SoeRequired for the Credential SLE — always serialized.
        let auto_accept = subject_id == issuer_id;
        let mut entry = serde_json::json!({
            "LedgerEntryType": "Credential",
            "Subject": subject_str,
            "Issuer": issuer_str,
            "CredentialType": credential_type,
            "Flags": if auto_accept { LSF_ACCEPTED } else { 0 },
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            entry["URI"] = serde_json::Value::String(uri.to_string());
        }
        if let Some(exp) = expiration {
            entry["Expiration"] = serde_json::Value::from(exp);
        }

        // Link into the issuer directory; the page it lands in is the SoeRequired
        // sfIssuerNode (always serialized, byte-critical for every credential).
        let issuer_node = add_to_owner_dir(ctx.view, &issuer_id, &cred_key)?;
        entry["IssuerNode"] = serde_json::Value::String(format!("{issuer_node:016X}"));
        helpers::adjust_owner_count(&mut account, 1);

        // For a third-party credential the subject directory is also linked
        // (ownership transfers to the subject on accept); record sfSubjectNode.
        // A self-issued credential links only the issuer dir and omits it.
        if !auto_accept {
            let subject_node = add_to_owner_dir(ctx.view, &subject_id, &cred_key)?;
            entry["SubjectNode"] = serde_json::Value::String(format!("{subject_node:016X}"));
        }

        let entry_data = serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(cred_key, entry_data)
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
    use rxrpl_amendment::feature::feature_id;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    /// Rules with the Credentials family amendments active, as on the oracle /
    /// modern mainnet.
    fn rules() -> Rules {
        Rules::from_enabled([feature_id("Credentials"), feature_id("fixInvalidTxFlags")])
    }

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
    fn preflight_amendment_disabled() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
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
            CredentialCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn preflight_invalid_flag() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "4B5943",
            "Flags": 0x00000001u32,
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
            CredentialCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemInvalidFlag)
        );
    }

    #[test]
    fn preflight_missing_subject() {
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
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
        let rules = rules();
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
    fn preflight_credential_type_too_long() {
        // 65 bytes (130 hex chars) > kMaxCredentialTypeLength (64).
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "AB".repeat(65),
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
            CredentialCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_uri_too_long() {
        // 257 bytes (514 hex chars) > kMaxCredentialUriLength (256).
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "4B5943",
            "URI": "AB".repeat(257),
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
            "CredentialType": "4B5943",
            "Flags": 0,
        });
        ledger
            .put_state(cred_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "4B5943",
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
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
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
        // A third-party credential starts unaccepted: Flags=0 (always serialized)
        // and BOTH IssuerNode and SubjectNode are written (single-page → "0").
        assert_eq!(entry["Flags"].as_u64().unwrap(), 0);
        assert_eq!(entry["IssuerNode"].as_str().unwrap(), "0000000000000000");
        assert_eq!(entry["SubjectNode"].as_str().unwrap(), "0000000000000000");

        let account_key = keylet::account(&alice_id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn apply_self_issued_is_accepted_no_subject_node() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        // issuer == subject (self-issued) → lsfAccepted, no SubjectNode.
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": ALICE,
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
            CredentialCreateTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let alice_id = decode_account_id(ALICE).unwrap();
        let cred_key = keylet::credential(&alice_id, &alice_id, b"KYC");
        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&cred_key).unwrap()).unwrap();
        assert_eq!(entry["Flags"].as_u64().unwrap() as u32, LSF_ACCEPTED);
        assert_eq!(entry["IssuerNode"].as_str().unwrap(), "0000000000000000");
        assert!(entry.get("SubjectNode").is_none());
    }

    #[test]
    fn apply_expired_returns_tec_expired() {
        let mut ledger = setup_two_accounts();
        ledger.header.parent_close_time = 1000;
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        // Expiration (500) strictly behind parent close time (1000) → tecEXPIRED.
        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
            "CredentialType": "4B5943",
            "Expiration": 500u32,
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
            CredentialCreateTransactor.apply(&mut ctx),
            Err(TransactionResult::TecExpired)
        );
    }

    #[test]
    fn apply_insufficient_reserve() {
        // Issuer balance below accountReserve(1) (= 12 XRP at default fees).
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ALICE, 5_000_000u64), (BOB, 50_000_000)] {
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
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "CredentialCreate",
            "Account": ALICE,
            "Subject": BOB,
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
            CredentialCreateTransactor.apply(&mut ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
    }
}
