use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::remove_from_owner_dir_keep_root;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct DIDDeleteTransactor;

impl Transactor for DIDDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if !ctx.rules.enabled(&feature_id("DID")) {
            return Err(TransactionResult::TemDisabled);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // The missing-DID check (tecNO_ENTRY) is a CLAIMED tec -- it must charge
        // the fee and consume the sequence -- so it runs in `apply`, which routes
        // through the engine's central fee/sequence consume. A tec returned from
        // preclaim would short-circuit before that consume and wrongly charge
        // nothing.
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // rippled DIDDelete::deleteSLE peeks the DID; absent -> tecNO_ENTRY, a
        // claimed tec (the engine already consumed fee + sequence centrally).
        let did_key = keylet::did(&account_id);
        if !ctx.view.exists(&did_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        // The engine consumed the sender's Sequence/fee centrally before apply,
        // so `account` is already post-fee/post-seq; only adjust its owner count.
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        remove_from_owner_dir_keep_root(ctx.view, &account_id, &did_key)?;
        ctx.view
            .erase(&did_key)
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

    fn setup_account_with_did() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();

        let account_key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(account_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let did_key = keylet::did(&id);
        let did = serde_json::json!({
            "LedgerEntryType": "DID",
            "Account": ALICE,
            "URI": "https://example.com",
            "Flags": 0,
        });
        ledger
            .put_state(did_key, serde_json::to_vec(&did).unwrap())
            .unwrap();

        ledger
    }

    fn setup_account_without_did() -> Ledger {
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
    fn preflight_ok_when_did_enabled() {
        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
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
        assert_eq!(DIDDeleteTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preflight_rejects_when_did_disabled() {
        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
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
            DIDDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn preclaim_no_did_passes() {
        // The missing-DID tecNO_ENTRY moved to apply (so the fee/seq are
        // claimed); preclaim now passes as long as the account exists.
        let ledger = setup_account_without_did();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(DIDDeleteTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn apply_missing_did_claims_tec() {
        let ledger = setup_account_without_did();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
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
            DIDDeleteTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn preclaim_with_did_ok() {
        let ledger = setup_account_with_did();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(DIDDeleteTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn apply_deletes_did() {
        let ledger = setup_account_with_did();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
            "Fee": "12",
            "Sequence": 1,
        });

        // Engine consumes the sender's Sequence centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = DIDDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let did_key = keylet::did(&id);
        assert!(!sandbox.exists(&did_key));

        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn apply_keeps_empty_owner_dir_root() {
        let mut ledger = setup_account_with_did();
        let id = decode_account_id(ALICE).unwrap();
        let did_key = keylet::did(&id);
        let dir_root = keylet::owner_dir(&id);
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Owner": ALICE,
            "RootIndex": dir_root.to_string().to_uppercase(),
            "Indexes": [did_key.to_string().to_uppercase()],
            "Flags": 0,
        });
        ledger
            .put_state(
                keylet::dir_node(&dir_root, 0),
                serde_json::to_vec(&dir).unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        DIDDeleteTransactor.apply(&mut ctx).unwrap();

        // rippled keeps the now-empty owner-directory root (keepRoot=true).
        let root_bytes = sandbox.read(&keylet::dir_node(&dir_root, 0)).unwrap();
        let root: serde_json::Value = serde_json::from_slice(&root_bytes).unwrap();
        assert_eq!(root["Indexes"].as_array().unwrap().len(), 0);
        assert!(!sandbox.exists(&did_key));
    }

    #[test]
    fn apply_relies_on_central_sequence_consume() {
        let ledger = setup_account_with_did();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": ALICE,
            "Fee": "12",
            "Sequence": 1,
        });

        // The handler no longer bumps the sequence itself; the central consume
        // does. Without it the sequence would stay at 1.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        DIDDeleteTransactor.apply(&mut ctx).unwrap();

        let id = decode_account_id(ALICE).unwrap();
        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["Sequence"].as_u64().unwrap(), 2);
    }
}
