use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

// Transaction flags
const TF_MPT_UNAUTHORIZE: u32 = 0x0001;

// Ledger entry flags
#[cfg(test)]
const LSFT_MPT_REQUIRE_AUTH: u32 = 0x0004;
const LSFT_MPT_AUTHORIZED: u32 = 0x0002;

pub struct MPTokenAuthorizeTransactor;

/// Parse MPTokenIssuanceID hex string into a Hash256 key.
fn parse_issuance_id(tx: &Value) -> Result<Hash256, TransactionResult> {
    let hex_str =
        helpers::get_str_field(tx, "MPTokenIssuanceID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Hash256::new(arr))
}

impl Transactor for MPTokenAuthorizeTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "MPTokenIssuanceID").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let issuance_key = parse_issuance_id(ctx.tx)?;
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let issuance: Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let issuer_str = issuance["Issuer"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;

        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);

        if account_str != issuer_str {
            // Holder flow
            let holder_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);

            if tx_flags & TF_MPT_UNAUTHORIZE == 0 {
                // Authorize (opt-in): MPToken must not already exist
                if ctx.view.exists(&mptoken_key) {
                    return Err(TransactionResult::TecDuplicate);
                }
            } else {
                // Unauthorize (opt-out): MPToken must exist with zero balance
                let mptoken_bytes = ctx
                    .view
                    .read(&mptoken_key)
                    .ok_or(TransactionResult::TecNoEntry)?;
                let mptoken: Value = serde_json::from_slice(&mptoken_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
                let amount = mptoken["MPTAmount"].as_str().unwrap_or("0");
                if amount != "0" {
                    return Err(TransactionResult::TecNoPermission);
                }
            }
        } else {
            // Issuer flow: MPTokenHolder required, MPToken must exist
            if helpers::get_str_field(ctx.tx, "MPTokenHolder").is_none() {
                return Err(TransactionResult::TemMalformed);
            }
            let holder_str = helpers::get_str_field(ctx.tx, "MPTokenHolder").unwrap();
            let holder_id = decode_account_id(holder_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
            if !ctx.view.exists(&mptoken_key) {
                return Err(TransactionResult::TecNoEntry);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let issuance_key = parse_issuance_id(ctx.tx)?;
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let issuance: Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let issuer_str = issuance["Issuer"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let issuance_id_hex = helpers::get_str_field(ctx.tx, "MPTokenIssuanceID")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();

        if account_str != issuer_str {
            // Holder flow
            let holder_id = &account_id;

            if tx_flags & TF_MPT_UNAUTHORIZE == 0 {
                // Create MPToken entry (holder opt-in)
                let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), holder_id);
                let mptoken = serde_json::json!({
                    "LedgerEntryType": "MPToken",
                    "Account": account_str,
                    "MPTokenIssuanceID": issuance_id_hex,
                    "MPTAmount": "0",
                    "Flags": 0,
                });
                let mptoken_data =
                    serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .insert(mptoken_key, mptoken_data)
                    .map_err(|_| TransactionResult::TefInternal)?;

                crate::owner_dir::add_to_owner_dir(ctx.view, holder_id, &mptoken_key)?;

                // +1 owner count on holder
                let acct_key = keylet::account(holder_id);
                let acct_bytes = ctx
                    .view
                    .read(&acct_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut acct: Value = serde_json::from_slice(&acct_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;

                helpers::increment_sequence(&mut acct);
                helpers::adjust_owner_count(&mut acct, 1);

                let acct_data =
                    serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(acct_key, acct_data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            } else {
                // Delete MPToken entry (holder opt-out)
                let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), holder_id);
                crate::owner_dir::remove_from_owner_dir(ctx.view, holder_id, &mptoken_key)?;
                ctx.view
                    .erase(&mptoken_key)
                    .map_err(|_| TransactionResult::TefInternal)?;

                // -1 owner count on holder
                let acct_key = keylet::account(holder_id);
                let acct_bytes = ctx
                    .view
                    .read(&acct_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut acct: Value = serde_json::from_slice(&acct_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;

                helpers::increment_sequence(&mut acct);
                helpers::adjust_owner_count(&mut acct, -1);

                let acct_data =
                    serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(acct_key, acct_data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        } else {
            // Issuer flow: authorize a holder
            let holder_str = helpers::get_str_field(ctx.tx, "MPTokenHolder")
                .ok_or(TransactionResult::TemMalformed)?;
            let holder_id = decode_account_id(holder_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;

            let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
            let mptoken_bytes = ctx
                .view
                .read(&mptoken_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut mptoken: Value = serde_json::from_slice(&mptoken_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

            let mut entry_flags = helpers::get_flags(&mptoken);
            entry_flags |= LSFT_MPT_AUTHORIZED;
            mptoken["Flags"] = Value::from(entry_flags);

            let mptoken_data =
                serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(mptoken_key, mptoken_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Update issuer account sequence
            let acct_key = keylet::account(&account_id);
            let acct_bytes = ctx
                .view
                .read(&acct_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut acct: Value =
                serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

            helpers::increment_sequence(&mut acct);

            let acct_data =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(acct_key, acct_data)
                .map_err(|_| TransactionResult::TefInternal)?;
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
    use rxrpl_ledger::Ledger;

    const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const HOLDER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ISSUER, 100_000_000u64), (HOLDER, 50_000_000)] {
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

    fn setup_with_issuance() -> (Ledger, Hash256) {
        let mut ledger = setup_accounts();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": ISSUER,
            "Sequence": 1,
            "MaximumAmount": "1000000",
            "TransferFee": 0,
            "AssetScale": 2,
            "OutstandingAmount": "0",
            "Flags": LSFT_MPT_REQUIRE_AUTH, // 0x0004
        });
        ledger
            .put_state(issuance_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, issuance_key)
    }

    fn issuance_id_hex(key: &Hash256) -> String {
        hex::encode(key.as_bytes()).to_uppercase()
    }

    #[test]
    fn holder_authorize_creates_mptoken() {
        let (ledger, issuance_key) = setup_with_issuance();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenAuthorizeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify MPToken entry created
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_bytes = sandbox.read(&mptoken_key).unwrap();
        let mptoken: Value = serde_json::from_slice(&mptoken_bytes).unwrap();
        assert_eq!(mptoken["Account"].as_str().unwrap(), HOLDER);
        assert_eq!(mptoken["MPTAmount"].as_str().unwrap(), "0");
        assert_eq!(mptoken["LedgerEntryType"].as_str().unwrap(), "MPToken");

        // Verify owner count incremented on holder
        let acct_key = keylet::account(&holder_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn holder_unauthorize_deletes_mptoken() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        // Create holder's MPToken entry
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_entry = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTAmount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(mptoken_key, serde_json::to_vec(&mptoken_entry).unwrap())
            .unwrap();
        // Set holder owner count to 1
        let acct_key = keylet::account(&holder_id);
        let acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": HOLDER,
            "Balance": "50000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(acct_key, serde_json::to_vec(&acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_UNAUTHORIZE,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenAuthorizeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify MPToken entry deleted
        assert!(sandbox.read(&mptoken_key).is_none());

        // Verify owner count decremented
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn issuer_authorizes_holder() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        // Create holder's MPToken entry (already opted in, not yet authorized)
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_entry = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTAmount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(mptoken_key, serde_json::to_vec(&mptoken_entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTokenHolder": HOLDER,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenAuthorizeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify lsfMPTAuthorized flag set on MPToken
        let mptoken_bytes = sandbox.read(&mptoken_key).unwrap();
        let mptoken: Value = serde_json::from_slice(&mptoken_bytes).unwrap();
        let flags = mptoken["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSFT_MPT_AUTHORIZED != 0);
    }

    #[test]
    fn reject_duplicate_holder_authorize() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        // MPToken already exists
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_entry = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTAmount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(mptoken_key, serde_json::to_vec(&mptoken_entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenAuthorizeTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }

    #[test]
    fn reject_unauthorize_nonzero_balance() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_entry = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTAmount": "500",
            "Flags": 0,
        });
        ledger
            .put_state(mptoken_key, serde_json::to_vec(&mptoken_entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_UNAUTHORIZE,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenAuthorizeTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn reject_missing_issuance_id() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenAuthorize",
            "Account": HOLDER,
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
            MPTokenAuthorizeTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
