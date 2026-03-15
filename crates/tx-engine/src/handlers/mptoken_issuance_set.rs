use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{keylet, TransactionResult};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

// Transaction flags
const TF_MPT_LOCK: u32 = 0x0001;
const TF_MPT_UNLOCK: u32 = 0x0002;

// Ledger entry flags
const LSFT_MPT_LOCKED: u32 = 0x0001;
const LSFT_MPT_CAN_LOCK: u32 = 0x0002;

pub struct MPTokenIssuanceSetTransactor;

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

impl Transactor for MPTokenIssuanceSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "MPTokenIssuanceID").is_none() {
            return Err(TransactionResult::TemMalformed);
        }

        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let has_lock = flags & TF_MPT_LOCK != 0;
        let has_unlock = flags & TF_MPT_UNLOCK != 0;

        if has_lock && has_unlock {
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

        // Issuer must match Account
        let issuer = issuance["Issuer"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if issuer != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // If locking/unlocking, issuance must have lsfMPTCanLock
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        if (flags & TF_MPT_LOCK != 0) || (flags & TF_MPT_UNLOCK != 0) {
            let issuance_flags = helpers::get_flags(&issuance);
            if issuance_flags & LSFT_MPT_CAN_LOCK == 0 {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let issuance_key = parse_issuance_id(ctx.tx)?;

        if let Some(holder_str) = helpers::get_str_field(ctx.tx, "MPTokenHolder") {
            // Lock/unlock a specific holder's MPToken entry
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
            if tx_flags & TF_MPT_LOCK != 0 {
                entry_flags |= LSFT_MPT_LOCKED;
            } else if tx_flags & TF_MPT_UNLOCK != 0 {
                entry_flags &= !LSFT_MPT_LOCKED;
            }
            mptoken["Flags"] = Value::from(entry_flags);

            let mptoken_data =
                serde_json::to_vec(&mptoken).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(mptoken_key, mptoken_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Lock/unlock the issuance itself
            let issuance_bytes = ctx
                .view
                .read(&issuance_key)
                .ok_or(TransactionResult::TecNoEntry)?;
            let mut issuance: Value = serde_json::from_slice(&issuance_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

            let mut entry_flags = helpers::get_flags(&issuance);
            if tx_flags & TF_MPT_LOCK != 0 {
                entry_flags |= LSFT_MPT_LOCKED;
            } else if tx_flags & TF_MPT_UNLOCK != 0 {
                entry_flags &= !LSFT_MPT_LOCKED;
            }
            issuance["Flags"] = Value::from(entry_flags);

            let issuance_data =
                serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(issuance_key, issuance_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Update account sequence
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
            "OutstandingAmount": "0",
            "Flags": LSFT_MPT_CAN_LOCK, // 0x0002
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
    fn lock_issuance() {
        let (ledger, issuance_key) = setup_with_issuance();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceSet",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_LOCK,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let issuance_bytes = sandbox.read(&issuance_key).unwrap();
        let issuance: Value = serde_json::from_slice(&issuance_bytes).unwrap();
        let flags = issuance["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSFT_MPT_LOCKED != 0);
    }

    #[test]
    fn unlock_issuance() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        // Set the issuance as locked
        let entry = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": ISSUER,
            "Sequence": 1,
            "OutstandingAmount": "0",
            "Flags": LSFT_MPT_CAN_LOCK | LSFT_MPT_LOCKED, // locked + can lock
        });
        ledger
            .put_state(issuance_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceSet",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_UNLOCK,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let issuance_bytes = sandbox.read(&issuance_key).unwrap();
        let issuance: Value = serde_json::from_slice(&issuance_bytes).unwrap();
        let flags = issuance["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSFT_MPT_LOCKED == 0);
        assert!(flags & LSFT_MPT_CAN_LOCK != 0);
    }

    #[test]
    fn lock_holder_mptoken() {
        let (mut ledger, issuance_key) = setup_with_issuance();
        // Create holder's MPToken entry
        let holder_id = decode_account_id(HOLDER).unwrap();
        let mptoken_key = keylet::mptoken(issuance_key.as_bytes(), &holder_id);
        let mptoken_entry = serde_json::json!({
            "LedgerEntryType": "MPToken",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTAmount": "100",
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
            "TransactionType": "MPTokenIssuanceSet",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "MPTokenHolder": HOLDER,
            "Flags": TF_MPT_LOCK,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let mptoken_bytes = sandbox.read(&mptoken_key).unwrap();
        let mptoken: Value = serde_json::from_slice(&mptoken_bytes).unwrap();
        let flags = mptoken["Flags"].as_u64().unwrap() as u32;
        assert!(flags & LSFT_MPT_LOCKED != 0);
    }

    #[test]
    fn reject_both_lock_and_unlock() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceSet",
            "Account": ISSUER,
            "MPTokenIssuanceID": "0000000000000000000000000000000000000000000000000000000000000000",
            "Flags": TF_MPT_LOCK | TF_MPT_UNLOCK,
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
            MPTokenIssuanceSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_lock_without_can_lock() {
        let mut ledger = setup_accounts();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": ISSUER,
            "Sequence": 1,
            "OutstandingAmount": "0",
            "Flags": 0, // no lsfMPTCanLock
        });
        ledger
            .put_state(issuance_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceSet",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_LOCK,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn reject_wrong_issuer() {
        let (ledger, issuance_key) = setup_with_issuance();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceSet",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Flags": TF_MPT_LOCK,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }
}
