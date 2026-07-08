use rxrpl_protocol::TransactionResult;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LedgerStateFixTransactor;

/// The only LedgerFixType rippled defines: fixNFTokenPageLinks.
const FIX_NFTOKEN_PAGE_LINK: u64 = 1;

impl Transactor for LedgerStateFixTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        match ctx.tx.get("LedgerFixType").and_then(|v| v.as_u64()) {
            Some(FIX_NFTOKEN_PAGE_LINK) => {
                if ctx.tx.get("Owner").and_then(|v| v.as_str()).is_none() {
                    return Err(TransactionResult::TemInvalid);
                }
                Ok(())
            }
            _ => Err(TransactionResult::TefInvalidLedgerFixType),
        }
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // fixNFTokenPageLinks repairs the pages owned by `Owner`, which must
        // exist (rippled returns tecOBJECT_NOT_FOUND otherwise).
        let owner = ctx
            .tx
            .get("Owner")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemInvalid)?;
        helpers::read_account_by_address(ctx.view, owner)
            .map_err(|_| TransactionResult::TecObjectNotFound)?;
        Ok(())
    }

    fn apply(&self, _ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        // The sender's fee and Sequence/Ticket are consumed centrally before
        // doApply. The actual NFTokenPage directory-link repair
        // (rippled's repairNFTokenDirectoryLinks) is not implemented, so this
        // does not modify the owner's pages — it only accepts a well-formed
        // request validated above.
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
    use rxrpl_codec::address::classic::decode_account_id;
    use rxrpl_ledger::Ledger;
    use rxrpl_protocol::keylet;

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

    fn preflight_of(tx: &serde_json::Value) -> Result<(), TransactionResult> {
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx,
            rules: &rules,
            fees: &fees,
        };
        LedgerStateFixTransactor.preflight(&ctx)
    }

    #[test]
    fn preflight_valid_type_with_owner_ok() {
        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 1,
            "Owner": ALICE,
            "Fee": "12",
        });
        assert_eq!(preflight_of(&tx), Ok(()));
    }

    #[test]
    fn preflight_unknown_type_rejected() {
        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 2,
            "Owner": ALICE,
            "Fee": "12",
        });
        assert_eq!(
            preflight_of(&tx),
            Err(TransactionResult::TefInvalidLedgerFixType)
        );
    }

    #[test]
    fn preflight_missing_type_rejected() {
        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "Fee": "12",
        });
        assert_eq!(
            preflight_of(&tx),
            Err(TransactionResult::TefInvalidLedgerFixType)
        );
    }

    #[test]
    fn preflight_type1_without_owner_rejected() {
        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 1,
            "Fee": "12",
        });
        assert_eq!(preflight_of(&tx), Err(TransactionResult::TemInvalid));
    }

    #[test]
    fn preclaim_owner_must_exist() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 1,
            "Owner": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTZ",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LedgerStateFixTransactor.preclaim(&ctx),
            Err(TransactionResult::TecObjectNotFound)
        );
    }

    #[test]
    fn preclaim_existing_owner_ok() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 1,
            "Owner": ALICE,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(LedgerStateFixTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn apply_increments_sequence() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
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

        let result = LedgerStateFixTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn apply_does_not_change_owner_count() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "LedgerStateFix",
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

        LedgerStateFixTransactor.apply(&mut ctx).unwrap();

        let id = decode_account_id(ALICE).unwrap();
        let account_key = keylet::account(&id);
        let account_bytes = sandbox.read(&account_key).unwrap();
        let account: serde_json::Value = serde_json::from_slice(&account_bytes).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
    }
}
