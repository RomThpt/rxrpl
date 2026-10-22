use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// Maximum byte length of a DID URI/DIDDocument/Data blob (rippled
/// kMaxDidUriLength / kMaxDidDocumentLength / kMaxDidDataLength).
const MAX_DID_FIELD_LENGTH: usize = 256;

/// Byte length of a DID blob field. The wire form is a hex-encoded VL blob, so
/// rippled's `length()` is the decoded byte count; fall back to the string
/// length for non-hex test inputs.
fn blob_byte_len(field: &str) -> usize {
    hex::decode(field).map(|b| b.len()).unwrap_or(field.len())
}

pub struct DIDSetTransactor;

impl Transactor for DIDSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if !ctx.rules.enabled(&feature_id("DID")) {
            return Err(TransactionResult::TemDisabled);
        }

        let uri = helpers::get_str_field(ctx.tx, "URI");
        let document = helpers::get_str_field(ctx.tx, "DIDDocument");
        let data = helpers::get_str_field(ctx.tx, "Data");

        // An empty transaction (none of the three fields present) is rejected.
        if uri.is_none() && document.is_none() && data.is_none() {
            return Err(TransactionResult::TemEmptyDid);
        }

        // All three fields present but all empty is also an empty DID. A single
        // present-but-empty field is allowed: on update it removes that field.
        if matches!(uri, Some("")) && matches!(document, Some("")) && matches!(data, Some("")) {
            return Err(TransactionResult::TemEmptyDid);
        }

        for field in [uri, document, data].into_iter().flatten() {
            if blob_byte_len(field) > MAX_DID_FIELD_LENGTH {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
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

        // The engine consumes the sender's Sequence/fee centrally before apply,
        // so `account` is already the post-fee, post-seq AccountRoot; the handler
        // only adjusts its owner count (on create).

        let did_key = keylet::did(&account_id);

        if ctx.view.exists(&did_key) {
            // Edit the existing DID (rippled DIDSet::doApply update branch).
            let entry_bytes = ctx
                .view
                .read(&did_key)
                .ok_or(TransactionResult::TefInternal)?;
            let mut entry: serde_json::Value =
                serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

            // A present field replaces the value; a present-but-empty field
            // removes it (rippled makeFieldAbsent); an absent field is untouched.
            for field in ["URI", "DIDDocument", "Data"] {
                if let Some(value) = helpers::get_str_field(ctx.tx, field) {
                    if value.is_empty() {
                        if let Some(obj) = entry.as_object_mut() {
                            obj.remove(field);
                        }
                    } else {
                        entry[field] = serde_json::Value::String(value.to_string());
                    }
                }
            }

            if entry.get("URI").is_none()
                && entry.get("DIDDocument").is_none()
                && entry.get("Data").is_none()
            {
                return Err(TransactionResult::TecEmptyDID);
            }

            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(did_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Create a new DID (rippled DIDSet::doApply + addSLE).
            let reserve = ctx
                .fees
                .account_reserve(helpers::get_owner_count(&account) + 1);
            if helpers::get_balance(&account) < reserve {
                return Err(TransactionResult::TecInsufficientReserve);
            }

            let mut entry = serde_json::json!({
                "LedgerEntryType": "DID",
                "Account": account_str,
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            for field in ["URI", "DIDDocument", "Data"] {
                if let Some(value) = helpers::get_str_field(ctx.tx, field) {
                    if !value.is_empty() {
                        entry[field] = serde_json::Value::String(value.to_string());
                    }
                }
            }

            if ctx.rules.enabled(&feature_id("fixEmptyDID"))
                && entry.get("URI").is_none()
                && entry.get("DIDDocument").is_none()
                && entry.get("Data").is_none()
            {
                return Err(TransactionResult::TecEmptyDID);
            }

            // rippled's DID node carries Flags=0 and the owner-directory page it
            // landed in (OwnerNode); both are serialized -- Flags is NOT dropped
            // (the node_binary is 2200000000...), unlike a Check.
            let owner_node = add_to_owner_dir(ctx.view, &account_id, &did_key)?;
            entry["Flags"] = serde_json::Value::from(0u32);
            entry["OwnerNode"] = serde_json::Value::String(format!("{owner_node:016X}"));

            let entry_data =
                serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(did_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, 1);
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
    use crate::transactor::{ApplyContext, PreflightContext};
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
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    fn did_rules() -> Rules {
        Rules::from_enabled([feature_id("DID")])
    }

    #[test]
    fn preflight_rejects_when_did_disabled() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "68747470733A2F2F6578616D706C652E636F6D",
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
            DIDSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn preflight_no_fields_is_empty_did() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "Fee": "12",
        });
        let rules = did_rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            DIDSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemEmptyDid)
        );
    }

    #[test]
    fn preflight_all_empty_is_empty_did() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "",
            "DIDDocument": "",
            "Data": "",
            "Fee": "12",
        });
        let rules = did_rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            DIDSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemEmptyDid)
        );
    }

    #[test]
    fn preflight_single_empty_field_allowed() {
        // A present-but-empty field means "remove that field" on update, so it
        // is accepted by preflight (unlike the old over-strict rejection).
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "DIDDocument": "",
            "Fee": "12",
        });
        let rules = did_rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(DIDSetTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preflight_rejects_too_long_field() {
        // 600 hex chars = 300 decoded bytes, over the 256-byte limit.
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "a".repeat(600),
            "Fee": "12",
        });
        let rules = did_rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            DIDSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_valid_single_field() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "68747470733A2F2F6578616D706C652E636F6D",
            "Fee": "12",
        });
        let rules = did_rules();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(DIDSetTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn apply_creates_did_entry() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "DIDDocument": "doc123",
            "URI": "https://example.com",
            "Fee": "12",
            "Sequence": 1,
        });

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = DIDSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let did_key = keylet::did(&id);
        assert!(sandbox.exists(&did_key));

        // The created DID carries Flags=0 and the owner-directory page hint
        // (OwnerNode) -- both serialized, matching rippled's node_binary.
        let did_bytes = sandbox.read(&did_key).unwrap();
        let did: serde_json::Value = serde_json::from_slice(&did_bytes).unwrap();
        assert_eq!(did["Flags"].as_u64().unwrap(), 0);
        assert_eq!(did["OwnerNode"].as_str().unwrap(), "0000000000000000");

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn apply_insufficient_reserve_claims_tec() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "5000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "68747470733A2F2F6578616D706C652E636F6D",
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
            DIDSetTransactor.apply(&mut ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
    }

    #[test]
    fn apply_update_removes_field() {
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let did_key = keylet::did(&id);
        let existing = serde_json::json!({
            "LedgerEntryType": "DID",
            "Account": ALICE,
            "URI": "68747470733A2F2F6578616D706C652E636F6D",
            "Data": "64617461",
            "Flags": 0,
            "OwnerNode": "0000000000000000",
        });
        ledger
            .put_state(did_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "",
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
            DIDSetTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let entry_bytes = sandbox.read(&did_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert!(entry.get("URI").is_none());
        assert_eq!(entry["Data"].as_str().unwrap(), "64617461");
    }

    #[test]
    fn apply_updates_existing_did() {
        let mut ledger = setup_account();
        let id = decode_account_id(ALICE).unwrap();
        let did_key = keylet::did(&id);
        let existing = serde_json::json!({
            "LedgerEntryType": "DID",
            "Account": ALICE,
            "URI": "old-uri",
            "Flags": 0,
        });
        ledger
            .put_state(did_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "URI": "new-uri",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = DIDSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let entry_bytes = sandbox.read(&did_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["URI"].as_str().unwrap(), "new-uri");

        // Owner count should not increase on update
        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
    }
}
