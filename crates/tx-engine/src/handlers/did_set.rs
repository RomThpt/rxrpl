use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct DIDSetTransactor;

impl Transactor for DIDSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let has_document = helpers::get_str_field(ctx.tx, "DIDDocument").is_some();
        let has_uri = helpers::get_str_field(ctx.tx, "URI").is_some();
        let has_data = helpers::get_str_field(ctx.tx, "Data").is_some();

        // At least one field must be present
        if !has_document && !has_uri && !has_data {
            return Err(TransactionResult::TemMalformed);
        }

        // Each present field must be non-empty
        if let Some(doc) = helpers::get_str_field(ctx.tx, "DIDDocument") {
            if doc.is_empty() {
                return Err(TransactionResult::TemMalformed);
            }
        }
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            if uri.is_empty() {
                return Err(TransactionResult::TemMalformed);
            }
        }
        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            if data.is_empty() {
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

        let did_key = keylet::did(&account_id);
        let is_create = !ctx.view.exists(&did_key);

        let mut entry = if is_create {
            serde_json::json!({
                "LedgerEntryType": "DID",
                "Account": account_str,
                "Flags": 0,
            })
        } else {
            let entry_bytes = ctx
                .view
                .read(&did_key)
                .ok_or(TransactionResult::TefInternal)?;
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?
        };

        if let Some(doc) = helpers::get_str_field(ctx.tx, "DIDDocument") {
            entry["DIDDocument"] = serde_json::Value::String(doc.to_string());
        }
        if let Some(uri) = helpers::get_str_field(ctx.tx, "URI") {
            entry["URI"] = serde_json::Value::String(uri.to_string());
        }
        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            entry["Data"] = serde_json::Value::String(data.to_string());
        }

        let entry_data =
            serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;

        if is_create {
            ctx.view
                .insert(did_key, entry_data)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut account, 1);
        } else {
            ctx.view
                .update(did_key, entry_data)
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
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_no_fields_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            DIDSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_string_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "DIDSet",
            "Account": ALICE,
            "DIDDocument": "",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
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
            "URI": "https://example.com",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
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

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 2);
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
