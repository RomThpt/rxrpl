use rxrpl_amendment::feature::feature_id;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PermissionedDomainDeleteTransactor;

/// DomainID is the 32-byte ledger key (uint256) of the PermissionedDomain.
fn parse_domain_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex = helpers::get_str_field(tx, "DomainID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex).map_err(|_| TransactionResult::TemMalformed)?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| TransactionResult::TemMalformed)?;
    Ok(Hash256::from(arr))
}

impl Transactor for PermissionedDomainDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if !ctx.rules.enabled(&feature_id("PermissionedDomains")) {
            return Err(TransactionResult::TemDisabled);
        }
        // sfDomainID must be present and non-zero.
        if parse_domain_id(ctx.tx)? == Hash256::ZERO {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // The sender must exist. The domain existence (tecNO_ENTRY) and ownership
        // (tecNO_PERMISSION) checks are deferred to apply so their `tec` results
        // are CLAIMED (fee + sequence charged), matching rippled.
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let domain_key = parse_domain_id(ctx.tx)?;

        // The domain must exist (claimed tecNO_ENTRY).
        let domain_bytes = ctx
            .view
            .read(&domain_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let domain: serde_json::Value =
            serde_json::from_slice(&domain_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Only the owner may delete the domain (claimed tecNO_PERMISSION).
        let owner_ok = domain
            .get("Owner")
            .and_then(|v| v.as_str())
            .and_then(|s| decode_account_id(s).ok())
            .map(|o| o == account_id)
            .unwrap_or(false);
        if !owner_ok {
            return Err(TransactionResult::TecNoPermission);
        }

        // Owner == Account: remove from the owner's directory (keeping the empty
        // root, matching rippled `dirRemove(..., keepRoot=true)`), decrement the
        // owner's owner count, and erase the domain.
        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        crate::owner_dir::remove_from_owner_dir_keep_root(ctx.view, &account_id, &domain_key)?;
        ctx.view
            .erase(&domain_key)
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
    // A second, real account (different owner) for the wrong-owner test.
    const BOB: &str = "rBu91aANPBsfQ9GR8dJ28CwKtnEVR4MMhN";

    fn rules() -> Rules {
        Rules::from_enabled([feature_id("PermissionedDomains"), feature_id("Credentials")])
    }

    fn domain_key() -> Hash256 {
        let id = decode_account_id(ALICE).unwrap();
        keylet::permissioned_domain(&id, 3)
    }

    fn domain_id_hex() -> String {
        hex::encode_upper(domain_key().as_bytes())
    }

    fn setup_account_with_domain(owner: &str) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();

        let account_key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 5,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(account_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let domain = serde_json::json!({
            "LedgerEntryType": "PermissionedDomain",
            "Owner": owner,
            "Sequence": 3,
            "AcceptedCredentials": [{"Credential": {"Issuer": ALICE, "CredentialType": "4B5943"}}],
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        ledger
            .put_state(domain_key(), serde_json::to_vec(&domain).unwrap())
            .unwrap();
        // Owner directory page (single entry on the root) so the delete unlinks it.
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Owner": ALICE,
            "Flags": 0,
            "RootIndex": hex::encode_upper(keylet::owner_dir(&id).as_bytes()),
            "Indexes": [domain_id_hex()],
        });
        ledger
            .put_state(keylet::owner_dir(&id), serde_json::to_vec(&dir).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_domain_id_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_zero_domain_id_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": "0000000000000000000000000000000000000000000000000000000000000000",
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_with_domain_id_ok() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = rules();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(PermissionedDomainDeleteTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preflight_amendment_disabled_rejects() {
        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
        });
        let fees = FeeSettings::default();
        let rules = Rules::new();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }

    #[test]
    fn apply_absent_domain_is_no_entry() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 5,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainDeleteTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_wrong_owner_is_no_permission() {
        // The domain is owned by BOB, but ALICE tries to delete it.
        let ledger = setup_account_with_domain(BOB);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            PermissionedDomainDeleteTransactor.apply(&mut ctx),
            Err(TransactionResult::TecNoPermission)
        );

        // The domain must still exist (the tec discards the erase).
        assert!(sandbox.exists(&domain_key()));
    }

    #[test]
    fn apply_deletes_domain() {
        let ledger = setup_account_with_domain(ALICE);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
            "Sequence": 5,
        });
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PermissionedDomainDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(ALICE).unwrap();
        assert!(!sandbox.exists(&domain_key()));

        let account_key = keylet::account(&id);
        let account: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&account_key).unwrap()).unwrap();
        assert_eq!(account["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(account["Sequence"].as_u64().unwrap(), 6);
    }

    #[test]
    fn preclaim_sender_exists_ok() {
        let ledger = setup_account_with_domain(ALICE);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = rules();

        let tx = serde_json::json!({
            "TransactionType": "PermissionedDomainDelete",
            "Account": ALICE,
            "DomainID": domain_id_hex(),
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(PermissionedDomainDeleteTransactor.preclaim(&ctx), Ok(()));
    }
}
