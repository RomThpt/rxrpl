use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// `lsfPasswordSpent` (0x00010000): armed on an AccountRoot when its owner
/// spends the one free (zero-fee) regular-key reset. rippled clears it again on
/// the next fee-paying transaction that credits the account (see Payment).
const LSF_PASSWORD_SPENT: u64 = 0x0001_0000;

/// SetRegularKey transaction handler.
///
/// Sets or clears the regular key pair for an account.
/// If RegularKey is present, sets it; if absent, clears it.
pub struct SetRegularKeyTransactor;

impl Transactor for SetRegularKeyTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // If RegularKey is provided, validate it's a valid address
        if let Some(key) = ctx.tx.get("RegularKey").and_then(|v| v.as_str()) {
            decode_account_id(key).map_err(|_| TransactionResult::TemBadRegKey)?;
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        if !ctx.view.exists(&key) {
            return Err(TransactionResult::TerNoAccount);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
        let mut obj: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        // Free (zero-fee) regular-key reset: rippled's SetRegularKey::doApply arms
        // lsfPasswordSpent whenever the required minimum fee is zero (see
        // SetRegularKey.cpp: `if (!minimumFee(...)) sle->setFlag(lsfPasswordSpent)`).
        // The base fee is zero only on the once-per-account free reset — signed by
        // the account's own key while lsfPasswordSpent is clear — so the account
        // pays a Fee of 0. Mirror that here: a Fee of 0 means the free path was
        // taken, so set the flag. A fee-paying SetRegularKey leaves Flags untouched.
        if helpers::get_fee(ctx.tx) == 0 {
            let flags = obj.get("Flags").and_then(Value::as_u64).unwrap_or(0);
            obj["Flags"] = Value::from(flags | LSF_PASSWORD_SPENT);
        }

        if let Some(reg_key) = ctx.tx.get("RegularKey") {
            obj["RegularKey"] = reg_key.clone();
        } else {
            obj.as_object_mut().unwrap().remove("RegularKey");
        }

        let new_bytes = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ACCT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const REG_KEY: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn ledger_with_account(addr: &str) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(addr).unwrap();
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    fn apply_set_regular_key(fee: &str) -> serde_json::Value {
        let ledger = ledger_with_account(ACCT);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "SetRegularKey",
            "Account": ACCT,
            "RegularKey": REG_KEY,
            "Fee": fee,
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetRegularKeyTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        let id = decode_account_id(ACCT).unwrap();
        let bytes = sandbox.read(&keylet::account(&id)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The free (Fee == 0) regular-key reset arms lsfPasswordSpent on the account,
    /// matching rippled SetRegularKey::doApply and the L29 oracle (Flags 0 -> 65536).
    #[test]
    fn free_reset_sets_password_spent() {
        let acct = apply_set_regular_key("0");
        assert_eq!(acct["RegularKey"].as_str().unwrap(), REG_KEY);
        assert_eq!(
            acct["Flags"].as_u64().unwrap() & LSF_PASSWORD_SPENT,
            LSF_PASSWORD_SPENT,
            "zero-fee SetRegularKey must set lsfPasswordSpent"
        );
    }

    /// A fee-paying SetRegularKey leaves Flags untouched (no lsfPasswordSpent),
    /// preserving mainnet neutrality for fee>0 key changes.
    #[test]
    fn fee_paying_leaves_flags_untouched() {
        let acct = apply_set_regular_key("12");
        assert_eq!(acct["RegularKey"].as_str().unwrap(), REG_KEY);
        assert_eq!(
            acct["Flags"].as_u64().unwrap(),
            0,
            "fee-paying SetRegularKey must not set lsfPasswordSpent"
        );
    }
}
