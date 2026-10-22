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

        let owner = ctx
            .tx
            .get("Owner")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemInvalid)?;
        helpers::read_account_by_address(ctx.view, owner)
            .map_err(|_| TransactionResult::TecObjectNotFound)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let owner = ctx
            .tx
            .get("Owner")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemInvalid)?;
        let owner_id = rxrpl_codec::address::classic::decode_account_id(owner)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // rippled fails a LedgerStateFix that had nothing to repair.
        if !crate::nftoken::repair_directory_links(ctx.view, &owner_id)? {
            return Err(TransactionResult::TecFailedProcessing);
        }
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
    use rxrpl_primitives::Hash256;
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

    fn alice_id() -> rxrpl_primitives::AccountId {
        decode_account_id(ALICE).unwrap()
    }

    fn put_page(ledger: &mut Ledger, key: &Hash256, page: &serde_json::Value) {
        ledger
            .put_state(*key, serde_json::to_vec(page).unwrap())
            .unwrap();
    }

    fn empty_page() -> serde_json::Value {
        serde_json::json!({ "LedgerEntryType": "NFTokenPage", "NFTokens": [] })
    }

    fn fix_tx() -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "LedgerStateFix",
            "Account": ALICE,
            "LedgerFixType": 1,
            "Owner": ALICE,
            "Fee": "12",
        })
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
        assert_eq!(preflight_of(&fix_tx()), Ok(()));
    }

    #[test]
    fn preflight_unknown_type_rejected() {
        let mut tx = fix_tx();
        tx["LedgerFixType"] = serde_json::json!(2);
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
        let mut tx = fix_tx();
        tx.as_object_mut().unwrap().remove("Owner");
        assert_eq!(preflight_of(&tx), Err(TransactionResult::TemInvalid));
    }

    #[test]
    fn preclaim_owner_must_exist() {
        let ledger = setup_account();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let mut tx = fix_tx();
        tx["Owner"] = serde_json::json!("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTZ");
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
        let tx = fix_tx();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(LedgerStateFixTransactor.preclaim(&ctx), Ok(()));
    }

    #[test]
    fn apply_no_damage_returns_tec_failed_processing() {
        let mut ledger = setup_account();
        let max = keylet::nftoken_page_max(&alice_id());
        put_page(&mut ledger, &max, &empty_page());

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = fix_tx();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            LedgerStateFixTransactor.apply(&mut ctx),
            Err(TransactionResult::TecFailedProcessing)
        );
    }

    #[test]
    fn apply_removes_stray_link_on_single_page() {
        let mut ledger = setup_account();
        let max = keylet::nftoken_page_max(&alice_id());
        let mut page = empty_page();
        page["NextPageMin"] =
            serde_json::json!("00000000000000000000000000000000000000000000000000000000000000AA");
        put_page(&mut ledger, &max, &page);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = fix_tx();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            LedgerStateFixTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );
        let repaired: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&max).unwrap()).unwrap();
        assert!(repaired.get("NextPageMin").is_none());
    }

    #[test]
    fn apply_relinks_two_pages() {
        let mut ledger = setup_account();
        let id = alice_id();
        let nft = Hash256::from_slice(&[0x11u8; 32]).unwrap();
        let first = keylet::nftoken_page(&id, &nft);
        let max = keylet::nftoken_page_max(&id);
        assert!(first < max, "first page must sort below the max page");

        put_page(&mut ledger, &first, &empty_page());
        put_page(&mut ledger, &max, &empty_page());

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = fix_tx();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            LedgerStateFixTransactor.apply(&mut ctx),
            Ok(TransactionResult::TesSuccess)
        );

        let p_first: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&first).unwrap()).unwrap();
        let p_max: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&max).unwrap()).unwrap();
        assert_eq!(
            p_first["NextPageMin"].as_str().unwrap(),
            max.to_string().to_uppercase()
        );
        assert_eq!(
            p_max["PreviousPageMin"].as_str().unwrap(),
            first.to_string().to_uppercase()
        );
        assert!(p_first.get("PreviousPageMin").is_none());
        assert!(p_max.get("NextPageMin").is_none());
    }
}
